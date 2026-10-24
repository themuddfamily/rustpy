//! Pure, bounded scalar and nominal-enum coverage over immutable acquired subjects.
//! These helpers never call inference or execute a guard.
use crate::types::LiteralValue;
use crate::{db::FileInfo, scope::BindingKind};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind, SyntaxNode,
};

pub fn pattern_at(file: &FileInfo, range: rustpy_core::TextRange) -> Option<SyntaxNode> {
    {
        let element = file.parse.syntax_node().covering_element(range);
        element.as_node().cloned().or_else(|| element.parent())?
    }
    .ancestors()
    .take(32)
    .find(|node| {
        node.text_range() == range
            && (node.kind().is_pattern() || node.kind() == SyntaxKind::MATCH_MAPPING_REST)
    })
}

pub fn integer_pattern(node: &SyntaxNode) -> Option<i64> {
    if node.kind() != SyntaxKind::MATCH_VALUE_PATTERN
        || node.text_range().len() > rustpy_core::TextSize::new(64)
    {
        return None;
    }
    let tokens: Vec<_> = node
        .children_with_tokens()
        .filter_map(|item| item.into_token())
        .filter(|token| !token.kind().is_trivia())
        .take(3)
        .collect();
    let text = match tokens.as_slice() {
        [number] if number.kind() == SyntaxKind::INT_NUMBER => number.text().to_owned(),
        [sign, number]
            if sign.kind() == SyntaxKind::MINUS && number.kind() == SyntaxKind::INT_NUMBER =>
        {
            format!("-{}", number.text())
        }
        _ => return None,
    };
    bounded_integer(&text)
}

/// Coverage after each case, only when the remaining domain is proven empty.
/// Unknown guards, patterns, annotations, mutations, sizes and aliases decline.
pub fn finite_integer_coverage(
    file: &FileInfo,
    statement: &ast::MatchStmt,
    canonical_literal: impl Fn(&str) -> bool,
) -> Option<Vec<bool>> {
    if !member_source_stable(file, "Literal") {
        return None;
    }
    let ast::Expr::Name(subject) = statement.subject()?.unparen() else {
        return None;
    };
    if !subject_name_stable(file, &subject.name_text()) {
        return None;
    }
    let scope = file.scopes.scope_at(subject.text_range().start());
    let ids = file.scopes.resolve(scope, &subject.name_text())?;
    if ids.len() != 1 {
        return None;
    }
    let binding = file.scopes.binding(ids[0]);
    if binding.kind != BindingKind::Param {
        return None;
    }
    let range = binding.annotation?;
    if range.len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    let root = file.parse.syntax_node();
    let element = root.covering_element(range);
    let node = element
        .as_node()
        .cloned()
        .or_else(|| element.parent())?
        .ancestors()
        .take(32)
        .find(|node| node.text_range() == range && ast::Expr::can_cast(node.kind()))?;
    let annotation = ast::Expr::cast(node)?;
    let domain = annotation_domain(file, &annotation, &canonical_literal, &mut 256, 0)?;
    finite_domain_coverage(
        statement,
        &domain,
        pattern_atom,
        |left, right, singleton| {
            Some(match (left, right) {
                (FinitePatternValue::None, FinitePatternValue::None) => true,
                (FinitePatternValue::Literal(left), FinitePatternValue::Literal(right)) => {
                    literal_pattern_equal(left, right, singleton)
                }
                _ => false,
            })
        },
    )
}

fn annotation_domain(
    file: &FileInfo,
    expression: &ast::Expr,
    canonical: &impl Fn(&str) -> bool,
    budget: &mut usize,
    depth: usize,
) -> Option<Vec<FinitePatternValue>> {
    if depth >= 16
        || *budget == 0
        || expression.text_range().len() > rustpy_core::TextSize::new(4096)
    {
        return None;
    }
    *budget -= 1;
    let expression = expression.clone().unparen();
    if let ast::Expr::Binary(binary) = &expression {
        if binary.op().map(|token| token.kind()) != Some(SyntaxKind::PIPE) {
            return None;
        }
        let mut values = annotation_domain(file, &binary.lhs()?, canonical, budget, depth + 1)?;
        for value in annotation_domain(file, &binary.rhs()?, canonical, budget, depth + 1)? {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        return (values.len() <= 128).then_some(values);
    }
    if matches!(&expression, ast::Expr::Literal(value) if value.kind()==Some(SyntaxKind::NONE_KW)) {
        return Some(vec![FinitePatternValue::None]);
    }
    if let ast::Expr::Name(name) = &expression {
        if name.name_text() == "bool"
            && canonical("builtins")
            && file
                .scopes
                .resolve(file.scopes.scope_at(name.text_range().start()), "bool")
                .is_none()
            && member_source_stable(file, "bool")
        {
            return Some(vec![
                FinitePatternValue::Literal(LiteralValue::Bool(false)),
                FinitePatternValue::Literal(LiteralValue::Bool(true)),
            ]);
        }
        return None;
    }
    let ast::Expr::Subscript(annotation) = expression else {
        return None;
    };
    let head = annotation.object()?.unparen();
    let (head_name, import) = match &head {
        ast::Expr::Name(head) => {
            let ids = file.scopes.resolve(
                file.scopes.scope_at(head.text_range().start()),
                &head.name_text(),
            )?;
            if ids.len() != 1 {
                return None;
            }
            let BindingKind::Import(import) = &file.scopes.binding(ids[0]).kind else {
                return None;
            };
            (import.name.as_deref()?.to_owned(), import)
        }
        ast::Expr::Attribute(head) => {
            let ast::Expr::Name(module) = head.object()?.unparen() else {
                return None;
            };
            let ids = file.scopes.resolve(
                file.scopes.scope_at(module.text_range().start()),
                &module.name_text(),
            )?;
            if ids.len() != 1 {
                return None;
            }
            let BindingKind::Import(import) = &file.scopes.binding(ids[0]).kind else {
                return None;
            };
            if import.name.is_some() {
                return None;
            }
            (head.attr_text(), import)
        }
        _ => return None,
    };
    if import.level != 0
        || !matches!(import.module.as_str(), "typing" | "typing_extensions")
        || !canonical(&import.module)
        || !member_source_stable(file, &head_name)
    {
        return None;
    }
    let index = annotation.index()?;
    let values = if index.kind() == SyntaxKind::SLICE_LIST {
        index
            .children()
            .take(33)
            .map(ast::Expr::cast)
            .collect::<Option<Vec<_>>>()?
    } else {
        match ast::Expr::cast(index)?.unparen() {
            ast::Expr::Tuple(tuple) => tuple.elements().take(33).collect(),
            other => vec![other],
        }
    };
    if values.is_empty() || values.len() > 32 || (head_name == "Optional" && values.len() != 1) {
        return None;
    }
    let mut domain = Vec::new();
    for value in values {
        let atoms = match head_name.as_str() {
            "Literal" => vec![expression_atom(&value)?],
            "Optional" | "Union" => annotation_domain(file, &value, canonical, budget, depth + 1)?,
            _ => return None,
        };
        for atom in atoms {
            if !domain.contains(&atom) {
                domain.push(atom);
            }
        }
        if domain.len() > 128 {
            return None;
        }
    }
    if head_name == "Optional" && !domain.contains(&FinitePatternValue::None) {
        domain.push(FinitePatternValue::None);
    }
    Some(domain)
}

/// Either independent full-domain proof may establish that a case has consumed
/// every residual subject. Unknown proofs never erase a successful proof.
pub fn merge_coverage(left: Option<Vec<bool>>, right: Option<Vec<bool>>) -> Option<Vec<bool>> {
    match (left, right) {
        (Some(mut left), Some(right)) if left.len() == right.len() => {
            for (left, right) in left.iter_mut().zip(right) {
                *left |= right;
            }
            Some(left)
        }
        (Some(left), _) => Some(left),
        (_, right) => right,
    }
}

/// Finite nominal enum coverage from immutable captured source/import edges.
/// Both semantic Flow and diagnostic consumers can use this without running
/// inference while walking a function body.
pub fn finite_enum_coverage(
    file: &FileInfo,
    statement: &ast::MatchStmt,
    imports: impl Fn(&FileInfo, crate::scope::BindingId) -> Option<std::sync::Arc<FileInfo>>,
) -> Option<Vec<bool>> {
    if file.text.len() > 1024 * 1024 {
        return None;
    }
    let ast::Expr::Name(subject) = statement.subject()?.unparen() else {
        return None;
    };
    if !subject_name_stable(file, &subject.name_text()) {
        return None;
    }
    let ids = file.scopes.resolve(
        file.scopes.scope_at(subject.text_range().start()),
        &subject.name_text(),
    )?;
    if ids.len() != 1 {
        return None;
    }
    let binding = file.scopes.binding(ids[0]);
    if binding.kind != BindingKind::Param {
        return None;
    }
    let range = binding.annotation?;
    if range.len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    let element = file.parse.syntax_node().covering_element(range);
    let expression = element
        .as_node()
        .cloned()
        .or_else(|| element.parent())?
        .ancestors()
        .take(32)
        .find(|node| node.text_range() == range)
        .and_then(ast::Expr::cast)?;
    let (owner, class) = crate::enum_authority::resolve_class(file, &expression, &imports)?;
    let model = crate::enum_authority::model(owner.as_deref().unwrap_or(file), &class, &imports)?;
    // Flag domains include combinations beyond their declared member names.
    if model.is_flag() || !crate::enum_authority::namespace_intact(file, &model) {
        return None;
    }
    let domain: Vec<_> = model
        .values()
        .into_iter()
        .map(FinitePatternValue::Literal)
        .collect();
    finite_domain_coverage(
        statement,
        &domain,
        |pattern| {
            if let Some(atom) = pattern_atom(pattern) {
                return Some(atom);
            }
            let names = dotted_pattern_names(pattern)?;
            if names.len() < 2 {
                return None;
            }
            let name = names.last()?;
            if !member_source_stable(file, name) {
                return None;
            }
            let (_, target) = crate::enum_authority::resolve_class_names(
                file,
                file.scopes.scope_at(pattern.text_range().start()),
                &names[..names.len() - 1],
                &imports,
            )?;
            if target != class {
                return None;
            }
            model.member(name).map(FinitePatternValue::Literal)
        },
        |left, right, singleton| match (left, right) {
            (FinitePatternValue::None, FinitePatternValue::None) => Some(true),
            (FinitePatternValue::Literal(left), FinitePatternValue::Literal(right)) => {
                crate::enum_authority::pattern_equal(left, right, singleton, &|target| {
                    (target == &class).then(|| model.clone())
                })
            }
            _ => Some(false),
        },
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinitePatternValue {
    None,
    Literal(LiteralValue),
}

/// Public captured-domain seam for the Flow authority bridge. Callers must
/// supply immutable admitted identities; this function never runs inference.
pub fn finite_domain_coverage(
    statement: &ast::MatchStmt,
    domain: &[FinitePatternValue],
    atom: impl Fn(&SyntaxNode) -> Option<FinitePatternValue>,
    equal: impl Fn(&FinitePatternValue, &FinitePatternValue, bool) -> Option<bool>,
) -> Option<Vec<bool>> {
    if domain.is_empty() || domain.len() > 128 {
        return None;
    }
    if domain.iter().any(|value|matches!(value,FinitePatternValue::Literal(LiteralValue::Str(text)|LiteralValue::Bytes(text)) if text.len()>4096)){return None;}
    let cases: Vec<_> = statement.cases().take(65).collect();
    if cases.len() > 64 {
        return None;
    }
    let mut remaining = domain.to_vec();
    let mut result = Vec::new();
    let mut budget = 16384usize;
    for case in cases {
        if case.guard().is_none()||case.guard().is_some_and(|guard|matches!(guard.unparen(),ast::Expr::Literal(value) if value.kind()==Some(SyntaxKind::TRUE_KW))) {
            let pattern=case.pattern()?;
            let mut next=Vec::new();
            for value in remaining {
                if accepts_atom(&pattern,&value,&atom,&equal,&mut budget,0).ok()?!=Some(true){next.push(value);}
            }
            remaining=next;
        }
        result.push(remaining.is_empty());
    }
    Some(result)
}
fn accepts_atom(
    pattern: &SyntaxNode,
    value: &FinitePatternValue,
    atom: &impl Fn(&SyntaxNode) -> Option<FinitePatternValue>,
    equal: &impl Fn(&FinitePatternValue, &FinitePatternValue, bool) -> Option<bool>,
    budget: &mut usize,
    depth: usize,
) -> Result<Option<bool>, ()> {
    if depth >= 16 || *budget == 0 || pattern.text_range().len() > rustpy_core::TextSize::new(4096)
    {
        return Err(());
    }
    *budget -= 1;
    match pattern.kind() {
        SyntaxKind::MATCH_WILDCARD_PATTERN | SyntaxKind::MATCH_CAPTURE_PATTERN => Ok(Some(true)),
        SyntaxKind::MATCH_GROUP_PATTERN | SyntaxKind::MATCH_AS_PATTERN => {
            let Some(inner) =
                pattern_children(pattern, 2).and_then(|items| items.into_iter().next())
            else {
                return Ok(None);
            };
            accepts_atom(&inner, value, atom, equal, budget, depth + 1)
        }
        SyntaxKind::MATCH_OR_PATTERN => {
            let Some(alternatives) = pattern_children(pattern, 32) else {
                return Err(());
            };
            let mut found = false;
            let mut uncertain = false;
            for alternative in alternatives {
                match accepts_atom(&alternative, value, atom, equal, budget, depth + 1)? {
                    Some(true) => found = true,
                    None => uncertain = true,
                    Some(false) => {}
                }
            }
            Ok(if uncertain { None } else { Some(found) })
        }
        SyntaxKind::MATCH_VALUE_PATTERN | SyntaxKind::MATCH_SINGLETON_PATTERN => Ok(atom(pattern)
            .and_then(|target| {
                equal(
                    value,
                    &target,
                    pattern.kind() == SyntaxKind::MATCH_SINGLETON_PATTERN,
                )
            })),
        _ => Ok(None),
    }
}

/// Shares the maintained syntax decoder, with a pre-allocation token cap.
pub fn expression_atom(expression: &ast::Expr) -> Option<FinitePatternValue> {
    if expression.text_range().len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    match expression.clone().unparen() {
        ast::Expr::Literal(value) => match value.kind() {
            Some(SyntaxKind::NONE_KW) => Some(FinitePatternValue::None),
            Some(SyntaxKind::TRUE_KW) => {
                Some(FinitePatternValue::Literal(LiteralValue::Bool(true)))
            }
            Some(SyntaxKind::FALSE_KW) => {
                Some(FinitePatternValue::Literal(LiteralValue::Bool(false)))
            }
            Some(SyntaxKind::INT_NUMBER)
                if value.text_range().len() <= rustpy_core::TextSize::new(64) =>
            {
                bounded_integer(&value.syntax().text().to_string())
                    .map(|integer| FinitePatternValue::Literal(LiteralValue::Int(integer)))
            }
            _ => None,
        },
        ast::Expr::String(value) => Some(FinitePatternValue::Literal(if value.is_bytes() {
            LiteralValue::Bytes(smol_str::SmolStr::new(value.value()))
        } else {
            LiteralValue::Str(smol_str::SmolStr::new(value.value()))
        })),
        _ => None,
    }
}
pub fn pattern_atom(pattern: &SyntaxNode) -> Option<FinitePatternValue> {
    if pattern.text_range().len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    if let Some(integer) = integer_pattern(pattern) {
        return Some(FinitePatternValue::Literal(LiteralValue::Int(integer)));
    }
    if pattern.kind() == SyntaxKind::MATCH_SINGLETON_PATTERN {
        return match pattern.first_token()?.kind() {
            SyntaxKind::NONE_KW => Some(FinitePatternValue::None),
            SyntaxKind::TRUE_KW => Some(FinitePatternValue::Literal(LiteralValue::Bool(true))),
            SyntaxKind::FALSE_KW => Some(FinitePatternValue::Literal(LiteralValue::Bool(false))),
            _ => None,
        };
    }
    let expressions: Vec<_> = pattern
        .children()
        .filter_map(ast::Expr::cast)
        .take(2)
        .collect();
    match expressions.as_slice() {
        [expression] => expression_atom(expression),
        _ => None,
    }
}

/// Bounded explicit member-retirement refusal. Unknown/oversized source cannot
/// certify a stored builtin/type-form member. False positives only lose proof.
pub fn member_source_stable(file: &FileInfo, member: &str) -> bool {
    member_source_stable_except(file, member, None)
}
pub fn member_source_stable_except(
    file: &FileInfo,
    member: &str,
    declaration: Option<rustpy_core::TextRange>,
) -> bool {
    if file.text.len() > 1024 * 1024 {
        return false;
    }
    // A pure function of the immutable source version: scan (up to 4096
    // nodes) once per member, not once per match statement or member query.
    let key = (smol_str::SmolStr::new(member), declaration);
    if let Some(&stable) = file.member_stability.lock().get(&key) {
        return stable;
    }
    let stable = scan_member_source_stable(file, member, declaration);
    file.member_stability.lock().insert(key, stable);
    stable
}

fn scan_member_source_stable(
    file: &FileInfo,
    member: &str,
    declaration: Option<rustpy_core::TextRange>,
) -> bool {
    let mut count = 0usize;
    for node in file.parse.syntax_node().descendants() {
        count += 1;
        if count > 4096 {
            return false;
        }
        // Explicit dynamic attribute writers can invalidate every field, even
        // when the object expression or attribute name is not statically known.
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            let writer = call.callee().is_some_and(|callee| match callee.unparen() {
                ast::Expr::Name(name) => matches!(name.name_text().as_str(), "setattr" | "delattr"),
                ast::Expr::Attribute(attribute) => matches!(
                    attribute.attr_text().as_str(),
                    "__setattr__" | "__delattr__"
                ),
                _ => false,
            });
            if writer {
                return false;
            }
        }
        let targets = if let Some(clause) = ast::CompFor::cast(node.clone()) {
            clause.target().into_iter().collect()
        } else if let Some(item) = ast::WithItem::cast(node.clone()) {
            item.target().into_iter().collect()
        } else {
            match ast::Stmt::cast(node) {
                Some(ast::Stmt::Assign(assign)) => assign.targets(),
                Some(ast::Stmt::AugAssign(assign)) => assign.target().into_iter().collect(),
                Some(ast::Stmt::AnnAssign(assign)) if assign.value().is_some() => {
                    assign.target().into_iter().collect()
                }
                Some(ast::Stmt::Del(delete)) => delete.targets(),
                Some(ast::Stmt::For(statement)) => statement.target().into_iter().collect(),
                _ => continue,
            }
        };
        let mut work = targets;

        while let Some(target) = work.pop() {
            count += 1;
            if count > 4096 {
                return false;
            }
            match target.unparen() {
                ast::Expr::Attribute(attribute)
                    if attribute.attr_text() == member
                        && attribute.attr().map(|name| name.text_range()) != declaration =>
                {
                    return false
                }
                ast::Expr::Star(star) => work.extend(star.expr()),
                ast::Expr::Tuple(tuple) => work.extend(tuple.elements().take(257)),
                ast::Expr::List(list) => work.extend(list.elements().take(257)),
                _ => {}
            }
            if work.len() > 256 {
                return false;
            }
        }
    }
    true
}

/// A pattern describes the acquired subject, not a name rebound by a guard.
/// Any visible same-name store (including nested global/nonlocal writers)
/// retires current-name projection. This deliberately admits fewer proofs.
pub fn subject_name_stable(file: &FileInfo, name: &str) -> bool {
    if file.text.len() > 1024 * 1024 || file.scopes.bindings.len() > 4096 {
        return false;
    }
    !file.scopes.bindings.iter().any(|binding| {
        binding.name == name
            && matches!(
                binding.kind,
                BindingKind::Assignment
                    | BindingKind::AugAssignment
                    | BindingKind::Walrus
                    | BindingKind::For
                    | BindingKind::With
                    | BindingKind::Except
                    | BindingKind::Del
                    | BindingKind::MatchCapture
                    | BindingKind::Function
                    | BindingKind::Class
                    | BindingKind::Import(_)
                    | BindingKind::Global
                    | BindingKind::Nonlocal
            )
    })
}

/// Bounded direct children; never silently truncate a pattern into a proof.
pub fn pattern_children(pattern: &SyntaxNode, limit: usize) -> Option<Vec<SyntaxNode>> {
    let children: Vec<_> = pattern
        .children()
        .filter(|child| child.kind().is_pattern())
        .take(limit + 1)
        .collect();
    (children.len() <= limit).then_some(children)
}
/// Value patterns use Python numeric equality; singleton patterns use identity.
pub fn literal_pattern_equal(
    left: &crate::types::LiteralValue,
    right: &crate::types::LiteralValue,
    singleton: bool,
) -> bool {
    use crate::types::LiteralValue;
    if !singleton {
        match (left, right) {
            (LiteralValue::Int(integer), LiteralValue::Bool(boolean))
            | (LiteralValue::Bool(boolean), LiteralValue::Int(integer)) => {
                return *integer == i64::from(*boolean)
            }
            _ => {}
        }
    }
    left == right
}

/// Bare/dotted value names have exact alternating token structure.
pub fn dotted_pattern_names(pattern: &SyntaxNode) -> Option<Vec<String>> {
    if pattern.text_range().len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    let tokens: Vec<_> = pattern
        .children_with_tokens()
        .filter_map(|item| item.into_token())
        .filter(|token| !token.kind().is_trivia())
        .take(18)
        .collect();
    if tokens.is_empty() || tokens.len() > 17 || tokens.len().is_multiple_of(2) {
        return None;
    }
    let mut names = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if index.is_multiple_of(2) {
            if token.kind() != SyntaxKind::IDENT || token.text().len() > 256 {
                return None;
            }
            names.push(token.text().to_owned());
        } else if token.kind() != SyntaxKind::DOT {
            return None;
        }
    }
    Some(names)
}
pub fn capture_name_at(pattern: &SyntaxNode, target: rustpy_core::TextSize) -> Option<String> {
    if pattern.text_range().len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    let mut tokens = pattern
        .descendants_with_tokens()
        .filter_map(|item| item.into_token())
        .take(4097);
    let token = tokens
        .find(|token| token.text_range().start() == target && token.kind() == SyntaxKind::IDENT)?;
    (token.text().len() <= 256).then(|| token.text().to_owned())
}
pub fn capture_offset(
    pattern: &SyntaxNode,
    name: &str,
    budget: &mut usize,
) -> Option<rustpy_core::TextSize> {
    let mut found = None;
    for node in pattern.descendants() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let token = match node.kind() {
            SyntaxKind::MATCH_CAPTURE_PATTERN
            | SyntaxKind::MATCH_STAR_PATTERN
            | SyntaxKind::MATCH_MAPPING_REST => ast::token(&node, SyntaxKind::IDENT),
            SyntaxKind::MATCH_AS_PATTERN => node
                .children_with_tokens()
                .filter_map(|item| item.into_token())
                .filter(|token| token.kind() == SyntaxKind::IDENT)
                .last(),
            _ => None,
        };
        if let Some(token) = token.filter(|token| token.text() == name) {
            if found.is_some() {
                return None;
            }
            found = Some(token.text_range().start());
        }
    }
    found
}

fn bounded_integer(text: &str) -> Option<i64> {
    if text.len() > 64 {
        return None;
    }
    let text = text.replace('_', "");
    let (negative, digits) = text
        .strip_prefix('-')
        .map_or((false, text.as_str()), |digits| (true, digits));
    let (radix, digits) = if let Some(digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, digits)
    } else if let Some(digits) = digits
        .strip_prefix("0o")
        .or_else(|| digits.strip_prefix("0O"))
    {
        (8, digits)
    } else if let Some(digits) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, digits)
    } else {
        (10, digits)
    };
    let value = u64::from_str_radix(digits, radix).ok()?;
    if negative {
        if value == (1u64 << 63) {
            Some(i64::MIN)
        } else {
            i64::try_from(value).ok()?.checked_neg()
        }
    } else {
        i64::try_from(value).ok()
    }
}

// Structural domains describe all values of an admitted annotation, not a
// guessed concrete runtime value. Only patterns covering the whole shape may
// retire its unmatched edge.
#[derive(Clone)]
enum PatternShape {
    Unknown,
    Tuple(Vec<PatternShape>),
    List,
    Mapping,
    Builtin(String),
    Class(rustpy_core::TextRange),
}

pub fn structural_coverage(
    file: &FileInfo,
    statement: &ast::MatchStmt,
    canonical: impl Fn(&str) -> bool,
) -> Option<Vec<bool>> {
    if file.text.len() > 1024 * 1024 {
        return None;
    }
    let ast::Expr::Name(subject) = statement.subject()?.unparen() else {
        return None;
    };
    if !subject_name_stable(file, &subject.name_text()) {
        return None;
    }
    let ids = file.scopes.resolve(
        file.scopes.scope_at(subject.text_range().start()),
        &subject.name_text(),
    )?;
    if ids.len() != 1 {
        return None;
    }
    let binding = file.scopes.binding(ids[0]);
    if binding.kind != BindingKind::Param {
        return None;
    }
    let range = binding.annotation?;
    if range.len() > rustpy_core::TextSize::new(4096) {
        return None;
    }
    let element = file.parse.syntax_node().covering_element(range);
    let expression = element
        .as_node()
        .cloned()
        .or_else(|| element.parent())?
        .ancestors()
        .take(32)
        .find(|node| node.text_range() == range)
        .and_then(ast::Expr::cast)?;
    let mut budget = 4096usize;
    let domain = shape_domain(file, &expression, &canonical, &mut budget, 0)?;
    let cases: Vec<_> = statement.cases().take(65).collect();
    if domain.len() > 128 || cases.len() > 64 {
        return None;
    }
    let mut live = vec![true; domain.len()];
    let mut result = Vec::new();
    for case in cases {
        if case.guard().is_none() || case.guard().is_some_and(|guard|matches!(guard.unparen(),ast::Expr::Literal(value) if value.kind()==Some(SyntaxKind::TRUE_KW))) {
            let pattern=case.pattern()?;
            for (index,shape) in domain.iter().enumerate() {
                if live[index] && shape_covered(file,shape,&pattern,&canonical,&mut budget,0)? {live[index]=false;}
            }
        }
        result.push(live.iter().all(|value| !*value));
    }
    Some(result)
}
fn shape_domain(
    file: &FileInfo,
    expression: &ast::Expr,
    canonical: &impl Fn(&str) -> bool,
    budget: &mut usize,
    depth: usize,
) -> Option<Vec<PatternShape>> {
    if depth >= 16 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let expression = expression.clone().unparen();
    if let ast::Expr::Binary(binary) = &expression {
        if binary.op().map(|token| token.kind()) != Some(SyntaxKind::PIPE) {
            return None;
        }
        let mut left = shape_domain(file, &binary.lhs()?, canonical, budget, depth + 1)?;
        left.extend(shape_domain(
            file,
            &binary.rhs()?,
            canonical,
            budget,
            depth + 1,
        )?);
        return (left.len() <= 128).then_some(left);
    }
    if let ast::Expr::Name(name) = &expression {
        let ids = file.scopes.resolve(
            file.scopes.scope_at(name.text_range().start()),
            &name.name_text(),
        );
        if let Some(ids) = ids {
            if ids.len() != 1 {
                return None;
            }
            let binding = file.scopes.binding(ids[0]);
            if binding.kind != BindingKind::Class {
                return None;
            }
            plain_class_shape(
                file,
                binding.node_range,
                canonical,
                budget,
                &mut Vec::new(),
                depth + 1,
            )?;
            return Some(vec![PatternShape::Class(binding.node_range)]);
        }
        if !canonical("builtins") || !member_source_stable(file, &name.name_text()) {
            return None;
        }
        return Some(vec![match name.name_text().as_str() {
            "list" => PatternShape::List,
            "dict" => PatternShape::Mapping,
            "int" | "str" | "bytes" | "bool" | "float" | "tuple" => {
                PatternShape::Builtin(name.name_text())
            }
            _ => return None,
        }]);
    }
    let ast::Expr::Subscript(subscript) = expression else {
        return None;
    };
    let ast::Expr::Name(head) = subscript.object()?.unparen() else {
        return None;
    };
    let name = head.name_text();
    if !canonical("builtins")
        || file
            .scopes
            .resolve(file.scopes.scope_at(head.text_range().start()), &name)
            .is_some()
        || !member_source_stable(file, &name)
    {
        return None;
    }
    match name.as_str() {
        "list" => Some(vec![PatternShape::List]),
        "dict" => Some(vec![PatternShape::Mapping]),
        "tuple" => {
            let index = subscript.index()?;
            let values: Vec<_> = if index.kind() == SyntaxKind::SLICE_LIST {
                index
                    .children()
                    .take(33)
                    .map(ast::Expr::cast)
                    .collect::<Option<Vec<_>>>()?
            } else {
                match ast::Expr::cast(index)?.unparen() {
                    ast::Expr::Tuple(tuple) => tuple.elements().take(33).collect(),
                    other => vec![other],
                }
            };
            if values.len() > 32 {
                return None;
            }
            if values.len() == 2
                && matches!(&values[1], ast::Expr::Literal(literal) if literal.kind()==Some(SyntaxKind::ELLIPSIS))
            {
                return Some(vec![PatternShape::Builtin("tuple".to_owned())]);
            }
            if values.iter().any(|value|matches!(value,ast::Expr::Literal(literal) if literal.kind()==Some(SyntaxKind::ELLIPSIS))) { return None; }
            let mut items = Vec::new();
            for value in values {
                let shapes = shape_domain(file, &value, canonical, budget, depth + 1);
                items.push(match shapes {
                    Some(mut shapes) if shapes.len() == 1 => shapes.remove(0),
                    _ => PatternShape::Unknown,
                });
            }
            Some(vec![PatternShape::Tuple(items)])
        }
        _ => None,
    }
}
/// A same-file physical ancestry proof. It never substitutes an inferred MRO
/// for unknown metaclass/base expressions and shares the caller's node budget.
fn plain_class_shape(
    file: &FileInfo,
    range: rustpy_core::TextRange,
    canonical: &impl Fn(&str) -> bool,
    budget: &mut usize,
    visiting: &mut Vec<rustpy_core::TextRange>,
    depth: usize,
) -> Option<()> {
    if depth >= 16 || *budget == 0 || visiting.contains(&range) {
        return None;
    }
    *budget -= 1;
    let element = file.parse.syntax_node().covering_element(range);
    let class = element
        .as_node()
        .cloned()
        .or_else(|| element.parent())?
        .ancestors()
        .take(32)
        .find_map(ast::ClassDef::cast)?;
    if class.text_range() != range || class.decorators().next().is_some() {
        return None;
    }
    visiting.push(range);
    if let Some(arguments) = class.arguments() {
        let bases: Vec<_> = arguments.args().take(33).collect();
        if bases.len() > 32 {
            return None;
        }
        for base in bases {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            let ast::AnyArg::Positional(base) = base else {
                return None;
            };
            let ast::Expr::Name(name) = base.expr()?.unparen() else {
                return None;
            };
            let ids = file.scopes.resolve(
                file.scopes.scope_at(name.text_range().start()),
                &name.name_text(),
            );
            if let Some(ids) = ids {
                if ids.len() != 1 {
                    return None;
                }
                let binding = file.scopes.binding(ids[0]);
                if binding.kind != BindingKind::Class
                    || binding.range.start() >= name.text_range().start()
                {
                    return None;
                }
                plain_class_shape(
                    file,
                    binding.node_range,
                    canonical,
                    budget,
                    visiting,
                    depth + 1,
                )?;
            } else if name.name_text() != "object"
                || !canonical("builtins")
                || !member_source_stable(file, "object")
            {
                return None;
            }
        }
    }
    visiting.pop();
    Some(())
}

fn shape_covered(
    file: &FileInfo,
    shape: &PatternShape,
    pattern: &SyntaxNode,
    canonical: &impl Fn(&str) -> bool,
    budget: &mut usize,
    depth: usize,
) -> Option<bool> {
    if depth >= 16 || *budget == 0 || pattern.text_range().len() > rustpy_core::TextSize::new(4096)
    {
        return None;
    }
    *budget -= 1;
    match pattern.kind() {
        SyntaxKind::MATCH_CAPTURE_PATTERN
        | SyntaxKind::MATCH_WILDCARD_PATTERN
        | SyntaxKind::MATCH_STAR_PATTERN => Some(true),
        SyntaxKind::MATCH_GROUP_PATTERN | SyntaxKind::MATCH_AS_PATTERN => shape_covered(
            file,
            shape,
            &pattern_children(pattern, 2)?.into_iter().next()?,
            canonical,
            budget,
            depth + 1,
        ),
        SyntaxKind::MATCH_OR_PATTERN => {
            for arm in pattern_children(pattern, 32)? {
                if shape_covered(file, shape, &arm, canonical, budget, depth + 1)? {
                    return Some(true);
                }
            }
            Some(false)
        }
        SyntaxKind::MATCH_SEQUENCE_PATTERN => {
            let items = pattern_children(pattern, 33)?;
            let stars: Vec<_> = items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.kind() == SyntaxKind::MATCH_STAR_PATTERN)
                .map(|(index, _)| index)
                .collect();
            if stars.len() > 1 {
                return Some(false);
            }
            if matches!(shape, PatternShape::List)
                || matches!(shape, PatternShape::Builtin(name) if name == "tuple")
            {
                return Some(items.len() == 1 && stars.len() == 1);
            }
            let PatternShape::Tuple(values) = shape else {
                return Some(false);
            };
            if values.len() < items.len() - stars.len()
                || (stars.is_empty() && values.len() != items.len())
            {
                return Some(false);
            }
            for (position, item) in items.iter().enumerate() {
                if stars.first() == Some(&position) {
                    continue;
                }
                let index = if stars.first().is_some_and(|star| position > *star) {
                    values.len() - (items.len() - position)
                } else {
                    position
                };
                if !shape_covered(file, &values[index], item, canonical, budget, depth + 1)? {
                    return Some(false);
                }
            }
            Some(true)
        }
        SyntaxKind::MATCH_MAPPING_PATTERN => Some(
            matches!(shape, PatternShape::Mapping)
                && !pattern
                    .children()
                    .any(|child| child.kind() == SyntaxKind::MATCH_MAPPING_ITEM),
        ),
        SyntaxKind::MATCH_CLASS_PATTERN => {
            if pattern.children().any(|child| {
                matches!(
                    child.kind(),
                    SyntaxKind::MATCH_CLASS_ARG | SyntaxKind::MATCH_KEYWORD_PATTERN
                )
            }) {
                return Some(false);
            }
            let names: Vec<_> = pattern
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.kind() == SyntaxKind::IDENT)
                .take(2)
                .collect();
            if names.len() != 1 {
                return Some(false);
            }
            let name = names[0].text();
            let ids = file
                .scopes
                .resolve(file.scopes.scope_at(pattern.text_range().start()), name);
            if let Some(ids) = ids {
                return Some(
                    ids.len() == 1
                        && matches!(shape,PatternShape::Class(range) if file.scopes.binding(ids[0]).kind==BindingKind::Class&&file.scopes.binding(ids[0]).node_range==*range),
                );
            }
            if !canonical("builtins") || !member_source_stable(file, name) {
                return Some(false);
            }
            Some(match shape {
                PatternShape::Tuple(_) => name == "tuple",
                PatternShape::List => name == "list",
                PatternShape::Mapping => name == "dict",
                PatternShape::Builtin(actual) => {
                    name == actual || (actual == "bool" && name == "int")
                }
                _ => false,
            })
        }
        _ => Some(false),
    }
}
