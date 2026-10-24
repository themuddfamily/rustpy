//! Bounded lexical binding roles; no inferred runtime/type authority.
use super::*;

#[derive(Default)]
struct Scope {
    names: FxHashSet<String>,
    globals: FxHashSet<String>,
    nonlocals: FxHashSet<String>,
    function: bool,
    stores: FxHashMap<String, Vec<TextSize>>,
    targets: FxHashSet<TextRange>,
}

fn owner(node: &SyntaxNode) -> Option<SyntaxNode> {
    for parent in node.ancestors().skip(1).take(64) {
        match parent.kind() {
            LAMBDA_EXPR | LIST_COMP | SET_COMP | DICT_COMP | GENERATOR_EXPR => return None,
            FUNC_DEF | CLASS_DEF => {
                // Defaults, bases and decorators belong to the enclosing scope.
                if parent.children().any(|child| {
                    child.kind() == BLOCK && child.text_range().contains_range(node.text_range())
                }) {
                    return Some(parent);
                }
            }
            MODULE => return Some(parent),
            _ => {}
        }
    }
    None
}

fn target_names(expression: Expr, out: &mut FxHashSet<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    match expression.unparen() {
        Expr::Name(name) => {
            out.insert(name.name_text());
        }
        Expr::Tuple(value) => {
            for item in value.elements().take(256) {
                target_names(item, out, depth + 1);
            }
        }
        Expr::List(value) => {
            for item in value.elements().take(256) {
                target_names(item, out, depth + 1);
            }
        }
        Expr::Star(value) => {
            if let Some(inner) = value.expr() {
                target_names(inner, out, depth + 1);
            }
        }
        // Attributes/subscripts are writes to an object, not local bindings.
        _ => {}
    }
}

pub(super) fn roles(root: &SyntaxNode) -> (FxHashSet<TextRange>, FxHashSet<TextRange>) {
    let nodes: Vec<_> = root.descendants().take(32769).collect();
    if nodes.len() > 32768 {
        return Default::default();
    }
    let mut scopes: FxHashMap<(rustpy_syntax::SyntaxKind, TextRange), Scope> = FxHashMap::default();
    for node in &nodes {
        let Some(scope_node) = owner(node) else {
            continue;
        };
        let scope = scopes
            .entry((scope_node.kind(), scope_node.text_range()))
            .or_default();
        scope.function = scope_node.kind() == FUNC_DEF;
        match node.kind() {
            GLOBAL_STMT | NONLOCAL_STMT => {
                let names = node
                    .children_with_tokens()
                    .filter_map(|x| x.into_token())
                    .filter(|x| x.kind() == IDENT)
                    .map(|x| x.text().to_string());
                if node.kind() == GLOBAL_STMT {
                    scope.globals.extend(names);
                } else {
                    scope.nonlocals.extend(names);
                }
            }
            ASSIGN_STMT => {
                if let Some(stmt) = ast::AssignStmt::cast(node.clone()) {
                    for target in stmt.targets() {
                        target_names(target, &mut scope.names, 0);
                    }
                }
            }
            ANN_ASSIGN_STMT => {
                if let Some(target) =
                    ast::AnnAssignStmt::cast(node.clone()).and_then(|x| x.target())
                {
                    target_names(target, &mut scope.names, 0);
                }
            }
            AUG_ASSIGN_STMT => {
                if let Some(target) =
                    ast::AugAssignStmt::cast(node.clone()).and_then(|x| x.target())
                {
                    target_names(target, &mut scope.names, 0);
                }
            }
            FOR_STMT => {
                if let Some(target) = ast::ForStmt::cast(node.clone()).and_then(|x| x.target()) {
                    target_names(target, &mut scope.names, 0);
                }
            }
            WITH_ITEM => {
                if let Some(target) = ast::WithItem::cast(node.clone()).and_then(|x| x.target()) {
                    target_names(target, &mut scope.names, 0);
                }
            }
            _ => {}
        }
        // Track actual stores at completion, not the whole dynamic namespace.
        // Record all target names even when a spelling was already bound.
        let mut stored = FxHashSet::default();
        match node.kind() {
            ASSIGN_STMT => {
                if let Some(stmt) = ast::AssignStmt::cast(node.clone()) {
                    for target in stmt.targets() {
                        target_names(target, &mut stored, 0);
                    }
                }
            }
            ANN_ASSIGN_STMT => {
                if let Some(stmt) = ast::AnnAssignStmt::cast(node.clone()) {
                    if stmt.value().is_some() {
                        if let Some(target) = stmt.target() {
                            target_names(target, &mut stored, 0);
                        }
                    }
                }
            }
            AUG_ASSIGN_STMT => {
                if let Some(target) =
                    ast::AugAssignStmt::cast(node.clone()).and_then(|x| x.target())
                {
                    target_names(target, &mut stored, 0);
                }
            }
            FOR_STMT => {
                if let Some(target) = ast::ForStmt::cast(node.clone()).and_then(|s| s.target()) {
                    target_names(target, &mut stored, 0);
                }
            }
            WITH_ITEM => {
                if let Some(target) = ast::WithItem::cast(node.clone()).and_then(|s| s.target()) {
                    target_names(target, &mut stored, 0);
                }
            }
            _ => {}
        }
        let store_point = if let Some(statement) = ast::ForStmt::cast(node.clone()) {
            statement
                .iter()
                .map(|value| value.text_range().end())
                .unwrap_or(node.text_range().end())
        } else {
            node.text_range().end()
        };
        for name in &stored {
            scope
                .stores
                .entry(name.clone())
                .or_default()
                .push(store_point);
        }
        for child in node.descendants().take(256).filter_map(ast::NameExpr::cast) {
            if stored.contains(&child.name_text()) {
                // Only target tokens; RHS reads are not writes.
                let is_target = match node.kind() {
                    ASSIGN_STMT => ast::AssignStmt::cast(node.clone()).is_some_and(|s| {
                        s.targets()
                            .iter()
                            .any(|t| t.text_range().contains_range(child.text_range()))
                    }),
                    ANN_ASSIGN_STMT => ast::AnnAssignStmt::cast(node.clone())
                        .and_then(|s| s.target())
                        .is_some_and(|t| t.text_range().contains_range(child.text_range())),
                    AUG_ASSIGN_STMT => ast::AugAssignStmt::cast(node.clone())
                        .and_then(|s| s.target())
                        .is_some_and(|t| t.text_range().contains_range(child.text_range())),
                    FOR_STMT => ast::ForStmt::cast(node.clone())
                        .and_then(|s| s.target())
                        .is_some_and(|t| t.text_range().contains_range(child.text_range())),
                    WITH_ITEM => ast::WithItem::cast(node.clone())
                        .and_then(|s| s.target())
                        .is_some_and(|t| t.text_range().contains_range(child.text_range())),
                    _ => false,
                };
                if is_target {
                    scope.targets.insert(child.text_range());
                }
            }
        }
    }
    for node in &nodes {
        if let Some(function) = ast::FuncDef::cast(node.clone()) {
            let own = scopes.entry((node.kind(), node.text_range())).or_default();
            own.function = true;
            if let Some(params) = function.params() {
                for param in params.params().take(256) {
                    if let Some(name) = param.name() {
                        own.names.insert(name.text().to_string());
                    }
                }
            }
            if let Some(parent) = owner(node) {
                scopes
                    .entry((parent.kind(), parent.text_range()))
                    .or_default()
                    .names
                    .insert(function.name_text());
            }
        } else if let Some(class) = ast::ClassDef::cast(node.clone()) {
            if let Some(parent) = owner(node) {
                scopes
                    .entry((parent.kind(), parent.text_range()))
                    .or_default()
                    .names
                    .insert(class.name_text());
            }
        } else if let Some(alias) = ast::ImportAlias::cast(node.clone()) {
            if let Some(parent) = owner(node) {
                if let Some(name) = alias.alias().or_else(|| alias.name_token()) {
                    scopes
                        .entry((parent.kind(), parent.text_range()))
                        .or_default()
                        .names
                        .insert(name.text().to_string());
                }
            }
        }
    }
    for node in &nodes {
        let binding = if let Some(function) = ast::FuncDef::cast(node.clone()) {
            Some(function.name_text())
        } else if let Some(class) = ast::ClassDef::cast(node.clone()) {
            Some(class.name_text())
        } else if let Some(alias) = ast::ImportAlias::cast(node.clone()) {
            alias
                .alias()
                .or_else(|| alias.name_token())
                .map(|x| x.text().to_string())
        } else {
            None
        };
        if let (Some(binding), Some(parent)) = (binding, owner(node)) {
            scopes
                .entry((parent.kind(), parent.text_range()))
                .or_default()
                .stores
                .entry(binding)
                .or_default()
                .push(node.text_range().end());
        }
    }
    let mut local = FxHashSet::default();
    let mut shadow = FxHashSet::default();
    for node in &nodes {
        let Some(name) = ast::NameExpr::cast(node.clone()) else {
            continue;
        };
        let Some(mut scope_node) = owner(node) else {
            continue;
        };
        let text = name.name_text();
        let deferred = node.ancestors().any(|parent| {
            parent.kind() == FUNC_DEF
                && parent.children().any(|child| {
                    child.kind() == BLOCK && child.text_range().contains_range(node.text_range())
                })
        });
        for _ in 0..64 {
            if let Some(scope) = scopes.get(&(scope_node.kind(), scope_node.text_range())) {
                let prior_store = scope.targets.contains(&name.text_range())
                    || scope.stores.get(&text).is_some_and(|points| {
                        points.iter().any(|p| *p <= name.text_range().start())
                    });
                if scope.globals.contains(&text) {
                    if prior_store {
                        shadow.insert(name.text_range());
                        break;
                    }
                    if let Some(module) = node.ancestors().find(|x| x.kind() == MODULE) {
                        if scopes
                            .get(&(module.kind(), module.text_range()))
                            .is_some_and(|x| {
                                (deferred && x.names.contains(&text))
                                    || x.stores.get(&text).is_some_and(|points| {
                                        points.iter().any(|p| *p <= name.text_range().start())
                                    })
                            })
                        {
                            shadow.insert(name.text_range());
                        }
                    }
                    break;
                }
                if !scope.nonlocals.contains(&text)
                    && scope.names.contains(&text)
                    && (scope.function || prior_store || (scope_node.kind() == MODULE && deferred))
                {
                    shadow.insert(name.text_range());
                    if scope.function
                        && !matches!(text.as_str(), "self" | "cls")
                        && !(text.len() > 4 && text.starts_with("__") && text.ends_with("__"))
                    {
                        local.insert(name.text_range());
                    }
                    break;
                }
            }
            if scope_node.kind() == CLASS_DEF
                && scopes
                    .get(&(scope_node.kind(), scope_node.text_range()))
                    .is_some_and(|scope| {
                        scope.names.contains(&text) && !scope.nonlocals.contains(&text)
                    })
            {
                // A class-assigned name uses LOAD_NAME, not a closure cell,
                // even before the class dictionary has received its store.
                let Some(module) = node.ancestors().find(|parent| parent.kind() == MODULE) else {
                    break;
                };
                scope_node = module;
                continue;
            }
            let Some(parent) = owner(&scope_node) else {
                break;
            };
            // Class dictionaries are not closure scopes for nested functions.
            if parent.kind() == CLASS_DEF {
                let Some(outer) = owner(&parent) else { break };
                scope_node = outer;
            } else {
                scope_node = parent;
            }
        }
    }
    (local, shadow)
}
