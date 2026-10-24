use super::*;
use std::collections::{HashMap, HashSet, VecDeque};
type BodyFingerprint = Vec<(SyntaxKind, String)>;
type DuplicateSites = Vec<(std::path::PathBuf, TextRange)>;
fn own_nodes(function: &ast::FuncDef) -> Vec<SyntaxNode> {
    let Some(body) = function.body() else {
        return Vec::new();
    };
    let mut stack = vec![body.syntax().clone()];
    let mut out = Vec::new();
    while let Some(node) = stack.pop() {
        if out.len() == 50000 {
            break;
        }
        let nested = matches!(
            node.kind(),
            SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
        );
        if !nested {
            stack.extend(node.children())
        }
        out.push(node)
    }
    out
}
pub(super) fn run(db: &Analysis, file: &Arc<FileInfo>, config: &StyleInspections, sink: &mut Sink) {
    if [
        config.max_arguments,
        config.max_branches,
        config.max_returns,
        config.max_statements,
        config.max_locals,
    ]
    .iter()
    .any(|n| *n > 0)
    {
        for function in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::FuncDef::cast)
        {
            let nodes = own_nodes(&function);
            let arguments = function.params().map_or(0, |params| {
                params
                    .params()
                    .filter_map(|p| p.name())
                    .filter(|name| !(function.is_method() && matches!(name.text(), "self" | "cls")))
                    .count()
            });
            let branches = nodes
                .iter()
                .filter(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::IF_STMT
                            | SyntaxKind::ELIF_CLAUSE
                            | SyntaxKind::FOR_STMT
                            | SyntaxKind::WHILE_STMT
                            | SyntaxKind::EXCEPT_CLAUSE
                            | SyntaxKind::CASE_CLAUSE
                    )
                })
                .count();
            let returns = nodes
                .iter()
                .filter(|node| node.kind() == SyntaxKind::RETURN_STMT)
                .count();
            let statements = nodes
                .iter()
                .filter(|node| ast::Stmt::can_cast(node.kind()))
                .count();
            let locals = file
                .scopes
                .scope_for_node(function.text_range())
                .map_or(0, |scope| {
                    file.scopes
                        .scope(scope)
                        .bindings
                        .iter()
                        .filter(|(_, ids)| {
                            ids.iter().any(|id| {
                                !matches!(
                                    file.scopes.binding(*id).kind,
                                    BindingKind::Param
                                        | BindingKind::StarParam
                                        | BindingKind::StarStarParam
                                        | BindingKind::Global
                                        | BindingKind::Nonlocal
                                )
                            })
                        })
                        .count()
                });
            for (label, value, limit) in [
                ("arguments", arguments, config.max_arguments),
                ("branches", branches, config.max_branches),
                ("returns", returns, config.max_returns),
                ("statements", statements, config.max_statements),
                ("local variables", locals, config.max_locals),
            ] {
                if limit > 0 && value > limit {
                    sink.push(
                        function
                            .name()
                            .map_or(function.text_range(), |name| name.text_range()),
                        Severity::WeakWarning,
                        "PyFunctionMetrics",
                        format!("Function has too many {label} ({value} > {limit})"),
                        Vec::new(),
                    )
                }
            }
        }
    }
    if config.duplicate_code {
        duplicates(db, file, config, sink)
    }
}
fn duplicates(db: &Analysis, current: &Arc<FileInfo>, config: &StyleInspections, sink: &mut Sink) {
    let width = config.duplicate_min_lines.clamp(2, 200);
    let minimum = config.duplicate_min_tokens.clamp(2, 5000);
    let mut files = HashMap::new();
    files.insert(current.path.clone(), current.clone());
    if let Some(loaded) = db.files_with_limit(1024) {
        for file in loaded {
            if !file.is_stub
                && db
                    .resolver
                    .roots
                    .iter()
                    .any(|root| file.path.starts_with(root))
            {
                files.insert(file.path.clone(), file);
            }
        }
    }
    // Bounded project discovery, loaded/unsaved text always wins. Frozen DBs
    // return no uncaptured files, keeping this query within their source world.
    let mut queue: VecDeque<_> = db.resolver.roots.iter().cloned().collect();
    let mut directories = 0;
    let mut entries = 0;
    while let Some(dir) = queue.pop_front() {
        directories += 1;
        if directories > 1024 || entries > 20000 || files.len() >= 512 {
            break;
        }
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in children.flatten() {
            entries += 1;
            if entries > 20000 {
                break;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !entry.file_name().to_string_lossy().starts_with('.')
                    && !matches!(
                        entry.file_name().to_str(),
                        Some("target" | "node_modules" | "__pycache__")
                    )
                {
                    queue.push_back(path)
                }
            } else if path.extension().is_some_and(|e| e == "py") && !files.contains_key(&path) {
                if let Some(file) = db.file_for_path(&path) {
                    files.insert(path, file);
                    if files.len() >= 512 {
                        break;
                    }
                }
            }
        }
    }
    let mut files: Vec<_> = files.into_values().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut groups: HashMap<BodyFingerprint, DuplicateSites> = HashMap::new();
    let mut total_bytes = 0;
    let mut total_tokens = 0;
    let mut fingerprint_budget = 200000usize;
    for file in files {
        total_bytes += file.text.len();
        if total_bytes > 8 * 1024 * 1024 {
            break;
        }
        if file.text.len() > 512 * 1024 {
            continue;
        }
        let (tokens, _) = rustpy_syntax::tokenize(&file.text);
        total_tokens += tokens.len();
        if total_tokens > 100000 {
            break;
        }
        let mut starts = vec![0usize];
        starts.extend(file.text.match_indices('\n').map(|(at, _)| at + 1));
        for line in 0..starts.len().saturating_sub(width) {
            let start = starts[line];
            let end = starts[line + width];
            let first = file.text[start..end].lines().next().unwrap_or("").trim();
            if first.is_empty()
                || first.starts_with(['#', '@'])
                || first.starts_with("import ")
                || first.starts_with("from ")
            {
                continue;
            }
            let mut key = Vec::new();
            let from = tokens.partition_point(|token| usize::from(token.range.start()) < start);
            let to = tokens.partition_point(|token| usize::from(token.range.start()) < end);
            for token in &tokens[from..to] {
                if fingerprint_budget == 0 {
                    return;
                }
                fingerprint_budget -= 1;
                if !matches!(
                    token.kind,
                    SyntaxKind::WHITESPACE
                        | SyntaxKind::NL
                        | SyntaxKind::NEWLINE
                        | SyntaxKind::COMMENT
                        | SyntaxKind::INDENT
                        | SyntaxKind::DEDENT
                        | SyntaxKind::EOF
                ) {
                    key.push((
                        token.kind,
                        file.text[usize::from(token.range.start())..usize::from(token.range.end())]
                            .into(),
                    ))
                }
            }
            if key.len() >= minimum {
                groups
                    .entry(key)
                    .or_default()
                    .push((file.path.clone(), TextRange::new(ts(start), ts(end))));
            }
        }
    }
    let mut emitted = HashSet::new();
    for locations in groups.into_values() {
        for (path, range) in &locations {
            if path != &current.path
                || emitted
                    .iter()
                    .any(|prior: &TextRange| prior.intersect(*range).is_some())
            {
                continue;
            }
            if let Some((other, at)) = locations.iter().find(|(other, at)| {
                other != path || at.end() <= range.start() || range.end() <= at.start()
            }) {
                sink.push(
                    *range,
                    Severity::WeakWarning,
                    "PyDuplicatedCode",
                    format!(
                        "Duplicated code fragment (also {} at byte {})",
                        other.display(),
                        u32::from(at.start())
                    ),
                    Vec::new(),
                );
                emitted.insert(*range);
            }
        }
    }
}
