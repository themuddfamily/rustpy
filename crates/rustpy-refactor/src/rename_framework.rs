//! Source identities for Django render-context and URL-name rename consumers.
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{rng, Edit},
    infer::Inferer,
    usages::{self, Symbol},
};
use rustpy_syntax::{
    ast::{self, AstNode},
    template::{self, Dialect, Kind},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
fn text(db: &Analysis, path: &std::path::Path) -> Option<String> {
    db.cached_file_for_path(path)
        .filter(|file| file.text.len() <= 1024 * 1024)
        .map(|file| file.text.to_string())
        .or_else(|| {
            use std::io::Read;
            let file = std::fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() > 1024 * 1024 {
                return None;
            }
            let mut text = String::new();
            file.take(1024 * 1024 + 1).read_to_string(&mut text).ok()?;
            (text.len() <= 1024 * 1024).then_some(text)
        })
}
fn same(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expression: &ast::Expr,
    source: &FileInfo,
    range: TextRange,
) -> bool {
    let expression = match expression {
        ast::Expr::Call(call) => {
            if let Some(ast::Expr::Attribute(attribute)) = call.callee() {
                if attribute.attr_text() == "as_view" {
                    attribute.object().unwrap_or_else(|| expression.clone())
                } else {
                    expression.clone()
                }
            } else {
                expression.clone()
            }
        }
        _ => expression.clone(),
    };
    let offset = match &expression {
        ast::Expr::Attribute(attribute) => attribute
            .attr()
            .map(|token| token.text_range().start())
            .unwrap_or_else(|| expression.syntax().text_range().start()),
        _ => expression.syntax().text_range().start(),
    };
    rustpy_semantic::features::definition_at(db, file, offset)
        .iter()
        .any(|location| location.path == source.path && location.range == range)
}
fn argument(call: &ast::CallExpr, index: usize, name: &str) -> Option<ast::Expr> {
    let arguments = call.arg_list()?;
    if arguments
        .args()
        .any(|arg| matches!(arg, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
    {
        return None;
    }
    let positional = arguments.positional().nth(index);
    let mut keywords = arguments
        .keywords()
        .filter(|argument| argument.name().is_some_and(|token| token.text() == name));
    let keyword = keywords.next().and_then(|argument| argument.value());
    if keywords.next().is_some() {
        return None;
    }
    if positional.is_some() && keyword.is_some() {
        None
    } else {
        positional.or(keyword)
    }
}
fn literal(expression: ast::Expr) -> Option<String> {
    if let ast::Expr::String(value) = expression {
        Some(value.value())
    } else {
        None
    }
}
fn dictionary(file: &FileInfo, expression: ast::Expr) -> Option<ast::DictExpr> {
    match expression {
        ast::Expr::Dict(dict) if dict.unpacks().next().is_none() => Some(dict),
        ast::Expr::Name(name) => {
            let ids = file.scopes.resolve(
                file.scopes.scope_at(name.syntax().text_range().start()),
                &name.name_text(),
            )?;
            if ids.len() != 1 {
                return None;
            }
            let binding = file.scopes.binding(ids[0]);
            if !matches!(
                binding.kind,
                rustpy_semantic::scope::BindingKind::Assignment
            ) || file
                .scopes
                .references_to(ids[0])
                .iter()
                .filter(|reference| !reference.is_write)
                .count()
                != 1
            {
                return None;
            }
            let range = binding.value?;
            file.parse.syntax_node().descendants().find_map(|node| {
                ast::DictExpr::cast(node).filter(|dict| {
                    dict.syntax().text_range() == range && dict.unpacks().next().is_none()
                })
            })
        }
        _ => None,
    }
}
fn template_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("html" | "jinja" | "jinja2" | "j2" | "django" | "htm")
            )
        })
        .cloned()
        .collect()
}
fn identifier_ranges(
    source: &str,
    range: std::ops::Range<usize>,
) -> Vec<(std::ops::Range<usize>, String)> {
    let mut result = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let character = source[at..].chars().next().unwrap();
        if matches!(character, '\'' | '"') {
            let quote = character;
            at += character.len_utf8();
            while at < range.end {
                let ch = source[at..].chars().next().unwrap();
                at += ch.len_utf8();
                if ch == '\\' {
                    if at < range.end {
                        at += source[at..].chars().next().unwrap().len_utf8();
                    }
                } else if ch == quote {
                    break;
                }
            }
            continue;
        }
        if rustpy_core::is_ident_start(character) {
            let start = at;
            at += character.len_utf8();
            while at < range.end
                && source[at..]
                    .chars()
                    .next()
                    .is_some_and(rustpy_core::is_ident_continue)
            {
                at += source[at..].chars().next().unwrap().len_utf8();
            }
            result.push((start..at, source[start..at].to_owned()));
        } else {
            at += character.len_utf8();
        }
    }
    result
}
fn local_names(document: &template::Document<'_>, region: usize) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let mut parent = document.regions[region].parent;
    let mut remaining = 128usize;
    while let Some(index) = parent {
        remaining = remaining.checked_sub(1)?;
        let Some(block) = document.blocks.get(index) else {
            break;
        };
        let opener = &document.regions[block.opener];
        let content = document.source[opener.content.clone()].trim();
        if let Some(rest) = content.strip_prefix("for ") {
            names.insert("forloop".into());
            if let Some((targets, _)) = rest.split_once(" in ") {
                for name in targets
                    .split(|ch: char| !rustpy_core::is_ident_continue(ch))
                    .filter(|name| !name.is_empty())
                {
                    names.insert(name.to_owned());
                }
            }
        }
        if let Some(rest) = content.strip_prefix("with ") {
            if let Some((_, name)) = rest.rsplit_once(" as ") {
                names.insert(name.trim().to_owned());
            }
            for part in rest.split_whitespace() {
                if let Some((name, _)) = part.split_once('=') {
                    names.insert(name.to_owned());
                }
            }
        }
        parent = block.parent;
    }
    Some(names)
}
fn root_uses(document: &template::Document<'_>, old: &str) -> Vec<TextRange> {
    let mut result = Vec::new();
    for (index, region) in document.regions.iter().enumerate() {
        if !matches!(region.kind, Kind::Expression | Kind::Statement)
            || local_names(document, index).is_none_or(|names| names.contains(old))
        {
            continue;
        }
        for (position, (range, name)) in identifier_ranges(document.source, region.content.clone())
            .into_iter()
            .enumerate()
        {
            if (region.kind == Kind::Statement && position == 0) || name != old {
                continue;
            }
            let prefix = &document.source[region.content.start..range.start];
            if prefix.trim_end().ends_with('.') || prefix.trim_end().ends_with('|') {
                continue;
            }
            if region.kind == Kind::Statement {
                let content = document.source[region.content.clone()].trim();
                if content.starts_with("block ")
                    || content.starts_with("endblock")
                    || content.starts_with("load ")
                    || content.starts_with("extends ")
                    || content.starts_with("include ")
                {
                    continue;
                }
                if content.starts_with("for ") {
                    if let Some(at) = document.source[region.content.clone()].find(" in ") {
                        if range.end <= region.content.start + at {
                            continue;
                        }
                    }
                }
                if content.starts_with("with ") || content.starts_with("url ") {
                    if let Some(at) = document.source[region.content.clone()].rfind(" as ") {
                        if range.start >= region.content.start + at + 4 {
                            continue;
                        }
                    }
                    if document.source[range.end..region.content.end]
                        .trim_start()
                        .starts_with('=')
                    {
                        continue;
                    }
                }
            }
            result.push(rng(range.start, range.end));
        }
    }
    result
}
#[derive(Clone, Debug)]
struct Url {
    name: String,
    key: ast::Expr,
    file: Arc<FileInfo>,
    view: ast::Expr,
}
fn urls(db: &Analysis, paths: &[PathBuf]) -> Vec<Url> {
    let mut result = Vec::new();
    for path in paths
        .iter()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("py"))
    {
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        let namespace = file
            .scopes
            .bindings_in(file.scopes.module_scope(), "app_name");
        let prefix = if namespace.len() == 1 {
            file.scopes
                .binding(namespace[0])
                .value
                .and_then(|range| {
                    file.parse.syntax_node().descendants().find_map(|node| {
                        ast::Expr::cast(node)
                            .filter(|expression| expression.syntax().text_range() == range)
                    })
                })
                .and_then(literal)
        } else {
            None
        };
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(callee) = call.callee() else {
                continue;
            };
            let Some((module, name)) =
                rustpy_semantic::template_context::canonical_import(db, &file, &callee)
            else {
                continue;
            };
            if !matches!(
                (module.as_str(), name.as_str()),
                ("django.urls", "path" | "re_path") | ("django.conf.urls", "url")
            ) {
                continue;
            }
            let Some(view) = argument(&call, 1, "view") else {
                continue;
            };
            let Some(key) = argument(&call, 3, "name") else {
                continue;
            };
            let Some(name) = literal(key.clone()) else {
                continue;
            };
            let name = prefix
                .as_ref()
                .map(|prefix| format!("{prefix}:{name}"))
                .unwrap_or(name);
            result.push(Url {
                name,
                key,
                file: file.clone(),
                view,
            });
        }
    }
    result
}
fn quoted_ranges(source: &str, range: std::ops::Range<usize>) -> Vec<(TextRange, String)> {
    let mut result = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let ch = source[at..].chars().next().unwrap();
        if !matches!(ch, '\'' | '"') {
            at += ch.len_utf8();
            continue;
        }
        let quote = ch;
        let start = at + 1;
        at = start;
        while at < range.end && !source[at..].starts_with(quote) {
            if source.as_bytes()[at] == b'\\' {
                break;
            }
            at += source[at..].chars().next().unwrap().len_utf8();
        }
        if at < range.end && source[at..].starts_with(quote) {
            result.push((rng(start, at), source[start..at].to_owned()));
            at += 1;
        } else {
            break;
        }
    }
    result
}
fn url_edits(
    db: &Analysis,
    registrations: &[Url],
    old: &str,
    new: &str,
    paths: &[PathBuf],
) -> Result<BTreeMap<PathBuf, Vec<Edit>>, String> {
    let selected: Vec<_> = registrations.iter().filter(|url| url.name == old).collect();
    if selected.len() != 1 {
        return Err("URL name is ambiguous or has no canonical registration".into());
    }
    let leaf = old.rsplit(':').next().unwrap_or(old);
    let renamed = old
        .rsplit_once(':')
        .map(|(namespace, _)| format!("{namespace}:{new}"))
        .unwrap_or_else(|| new.to_owned());
    if registrations.iter().any(|url| url.name == renamed) {
        return Err("The URL name already exists".into());
    }
    let mut result = BTreeMap::<PathBuf, Vec<Edit>>::new();
    let url = selected[0];
    if let Some(range) = crate::rename_family::literal_name_range(&url.file, &url.key, leaf) {
        result
            .entry(url.file.path.clone())
            .or_default()
            .push((range, new.to_owned()));
    }
    for path in paths {
        if path.extension().and_then(|extension| extension.to_str()) == Some("py") {
            let Some(file) = db.file_for_path(path) else {
                continue;
            };
            for call in file
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::CallExpr::cast)
            {
                let Some(callee) = call.callee() else {
                    continue;
                };
                let Some((module, name)) =
                    rustpy_semantic::template_context::canonical_import(db, &file, &callee)
                else {
                    continue;
                };
                if !matches!(
                    (module.as_str(), name.as_str()),
                    ("django.urls", "reverse" | "reverse_lazy") | ("django.shortcuts", "redirect")
                ) {
                    continue;
                }
                if let Some(key) = argument(&call, 0, "viewname") {
                    if let Some(range) = crate::rename_family::literal_name_range(&file, &key, old)
                    {
                        let range = TextRange::new(
                            range.end() - TextSize::new(leaf.len() as u32),
                            range.end(),
                        );
                        result
                            .entry(path.clone())
                            .or_default()
                            .push((range, new.to_owned()));
                    }
                }
            }
        }
    }
    for path in template_paths(paths) {
        let Some(source) = text(db, &path) else {
            continue;
        };
        let document = template::parse(&source, Dialect::Django);
        for region in &document.regions {
            if region.kind != Kind::Statement
                || !source[region.content.clone()]
                    .trim_start()
                    .starts_with("url ")
            {
                continue;
            }
            if let Some((range, name)) = quoted_ranges(&source, region.content.clone()).first() {
                if name == old {
                    let range =
                        TextRange::new(range.end() - TextSize::new(leaf.len() as u32), range.end());
                    result
                        .entry(path.clone())
                        .or_default()
                        .push((range, new.to_owned()));
                }
            }
        }
    }
    Ok(result)
}
/// Ordered identity for parsed byte ranges; TextRange deliberately has no Ord.
fn root_use_offsets(document: &template::Document<'_>, name: &str) -> BTreeSet<(usize, usize)> {
    root_uses(document, name)
        .into_iter()
        .map(|range| (usize::from(range.start()), usize::from(range.end())))
        .collect()
}
pub fn related_edits(
    db: &Analysis,
    symbol: &Symbol,
    new_name: &str,
    paths: &[PathBuf],
) -> Result<Vec<(PathBuf, Vec<Edit>)>, String> {
    let Some((owner, range)) = crate::rename_family::target(db, symbol) else {
        return Ok(Vec::new());
    };
    let old = symbol.name();
    let templates = template_paths(paths);
    let mut result = BTreeMap::<PathBuf, Vec<Edit>>::new();
    // A template used with different contexts has no single root/member
    // identity. Do not rewrite its shared source from one render call alone.
    let mut render_counts = BTreeMap::<String, usize>::new();
    for path in paths
        .iter()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("py"))
    {
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(callee) = call.callee() else {
                continue;
            };
            let Some((module, name)) =
                rustpy_semantic::template_context::canonical_import(db, &file, &callee)
            else {
                continue;
            };
            let index = match (module.as_str(), name.as_str()) {
                ("django.shortcuts", "render") => 1,
                ("django.template.loader", "render_to_string") => 0,
                _ => continue,
            };
            if let Some(name) = argument(&call, index, "template_name").and_then(literal) {
                *render_counts.entry(name).or_default() += 1;
            }
        }
    }
    for path in paths
        .iter()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("py"))
    {
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(callee) = call.callee() else {
                continue;
            };
            let Some((module, name)) =
                rustpy_semantic::template_context::canonical_import(db, &file, &callee)
            else {
                continue;
            };
            let (template_index, context_index) = match (module.as_str(), name.as_str()) {
                ("django.shortcuts", "render") => (1, 2),
                ("django.template.loader", "render_to_string") => (0, 1),
                _ => continue,
            };
            let Some(template) = argument(&call, template_index, "template_name").and_then(literal)
            else {
                continue;
            };
            if render_counts.get(&template) != Some(&1) {
                continue;
            }
            let candidates = templates
                .iter()
                .filter(|path| path.ends_with(&template))
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                continue;
            }
            let Some(context) = argument(&call, context_index, "context")
                .and_then(|expression| dictionary(&file, expression))
            else {
                continue;
            };
            let template_path = candidates[0];
            let Some(source) = text(db, template_path) else {
                continue;
            };
            let document = template::parse(&source, Dialect::Django);
            for item in context.items() {
                let (Some(key), Some(value)) = (item.key(), item.value()) else {
                    continue;
                };
                let Some(key_name) = literal(key.clone()) else {
                    continue;
                };
                if key_name == old && same(db, &file, &value, &owner, range) {
                    if let Some(key_range) =
                        crate::rename_family::literal_name_range(&file, &key, old)
                    {
                        result
                            .entry(file.path.clone())
                            .or_default()
                            .push((key_range, new_name.to_owned()));
                        for range in root_uses(&document, old) {
                            result
                                .entry(template_path.clone())
                                .or_default()
                                .push((range, new_name.to_owned()));
                        }
                    }
                }
                // Attribute chains carry actual context-value types. Django's
                // fallback method invocation/dict-key rules never grant identity.
                let context_type = Inferer::new(db).type_of_expr(&file, &value);
                let context_roots = root_use_offsets(&document, &key_name);
                for (index, region) in
                    document.regions.iter().enumerate().filter(|(_, region)| {
                        matches!(region.kind, Kind::Expression | Kind::Statement)
                    })
                {
                    if local_names(&document, index).is_none_or(|names| names.contains(&key_name)) {
                        continue;
                    }
                    let words = identifier_ranges(&source, region.content.clone());
                    for root_index in 0..words.len() {
                        if words[root_index].1 != key_name
                            || !context_roots
                                .contains(&(words[root_index].0.start, words[root_index].0.end))
                        {
                            continue;
                        }
                        let mut owner_type = context_type.clone();
                        let mut previous = words[root_index].0.end;
                        for (word_range, word) in words.iter().skip(root_index + 1).take(32) {
                            if source[previous..word_range.start].trim() != "." {
                                break;
                            }
                            if word == old
                                && usages::member_declaration(db, &owner_type, old).is_some_and(
                                    |(source, declaration)| {
                                        source.id == owner.id && declaration == range
                                    },
                                )
                            {
                                result.entry(template_path.clone()).or_default().push((
                                    rng(word_range.start, word_range.end),
                                    new_name.to_owned(),
                                ));
                            }
                            let Some(next) = Inferer::new(db).lookup_member(&owner_type, word)
                            else {
                                break;
                            };
                            owner_type = next;
                            previous = word_range.end;
                        }
                    }
                }
            }
        }
    }
    let registrations = urls(db, paths);
    for url in &registrations {
        let leaf = url.name.rsplit(':').next().unwrap_or(&url.name);
        if leaf == old && same(db, &url.file, &url.view, &owner, range) {
            for (path, edits) in url_edits(db, &registrations, &url.name, new_name, paths)? {
                result.entry(path).or_default().extend(edits);
            }
        }
    }
    Ok(result.into_iter().collect())
}

// A template root is declared by a unique canonical render context key,
// independently of the spelling of the Python value supplying that key.
fn context_key(
    db: &Analysis,
    template_path: &std::path::Path,
    name: &str,
    paths: &[PathBuf],
) -> Option<(Arc<FileInfo>, ast::DictExpr, ast::Expr)> {
    let templates = template_paths(paths);
    let mut renders = 0usize;
    let mut selected = None;
    for path in paths
        .iter()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("py"))
    {
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(callee) = call.callee() else {
                continue;
            };
            let Some((module, callee_name)) =
                rustpy_semantic::template_context::canonical_import(db, &file, &callee)
            else {
                continue;
            };
            let (template_index, context_index) = match (module.as_str(), callee_name.as_str()) {
                ("django.shortcuts", "render") => (1, 2),
                ("django.template.loader", "render_to_string") => (0, 1),
                _ => continue,
            };
            let Some(template) = argument(&call, template_index, "template_name").and_then(literal)
            else {
                continue;
            };
            let candidates = templates
                .iter()
                .filter(|path| path.ends_with(&template))
                .collect::<Vec<_>>();
            if candidates.len() != 1 || candidates[0].as_path() != template_path {
                continue;
            }
            renders += 1;
            let Some(context) = argument(&call, context_index, "context")
                .and_then(|value| dictionary(&file, value))
            else {
                continue;
            };
            let keys = context
                .items()
                .filter_map(|item| item.key())
                .filter(|key| literal(key.clone()).as_deref() == Some(name))
                .collect::<Vec<_>>();
            if keys.len() == 1 {
                selected = Some((file.clone(), context, keys[0].clone()));
            }
        }
    }
    (renders == 1).then_some(selected).flatten()
}

#[derive(Clone, Debug)]
pub enum FrameworkTarget {
    Block {
        path: PathBuf,
        name: String,
    },
    Url {
        name: String,
    },
    Context {
        path: PathBuf,
        name: String,
        source: PathBuf,
        key: TextRange,
    },
}
impl FrameworkTarget {
    pub fn name(&self) -> &str {
        match self {
            Self::Block { name, .. } | Self::Context { name, .. } => name,
            Self::Url { name } => name.rsplit(':').next().unwrap_or(name),
        }
    }
}
pub fn target_at(
    db: &Analysis,
    path: &std::path::Path,
    source: &str,
    offset: TextSize,
    paths: &[PathBuf],
) -> Option<FrameworkTarget> {
    if matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("html" | "htm" | "jinja" | "jinja2" | "j2" | "django")
    ) {
        let document = template::parse(source, Dialect::Django);
        for region in &document.regions {
            if region.range.contains(&usize::from(offset)) {
                for (range, name) in identifier_ranges(source, region.content.clone()) {
                    if range.contains(&usize::from(offset))
                        && root_uses(&document, &name).contains(&rng(range.start, range.end))
                    {
                        if let Some((file, _, key)) = context_key(db, path, &name, paths) {
                            if let Some(key) =
                                crate::rename_family::literal_name_range(&file, &key, &name)
                            {
                                return Some(FrameworkTarget::Context {
                                    path: path.to_owned(),
                                    name,
                                    source: file.path.clone(),
                                    key,
                                });
                            }
                        }
                    }
                }
            }
            if region.kind != Kind::Statement || !region.range.contains(&usize::from(offset)) {
                continue;
            }
            let words = identifier_ranges(source, region.content.clone());
            if words
                .first()
                .is_some_and(|(_, word)| matches!(word.as_str(), "block" | "endblock"))
            {
                let (range, name) = words.get(1)?;
                if range.contains(&usize::from(offset)) {
                    return Some(FrameworkTarget::Block {
                        path: path.to_owned(),
                        name: name.clone(),
                    });
                }
            }
            if source[region.content.clone()]
                .trim_start()
                .starts_with("url ")
            {
                for (range, name) in quoted_ranges(source, region.content.clone()) {
                    if range.contains(offset) {
                        let registrations = urls(db, paths);
                        if registrations.iter().filter(|url| url.name == name).count() == 1 {
                            return Some(FrameworkTarget::Url { name });
                        }
                    }
                }
            }
        }
    } else if path.extension().and_then(|extension| extension.to_str()) == Some("py") {
        let file = db.file_for_path(path)?;
        let registrations = urls(db, paths);
        for url in &registrations {
            if url.file.path == path && url.key.syntax().text_range().contains(offset) {
                return Some(FrameworkTarget::Url {
                    name: url.name.clone(),
                });
            }
        }
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(callee) = call.callee() else {
                continue;
            };
            let Some((module, name)) =
                rustpy_semantic::template_context::canonical_import(db, &file, &callee)
            else {
                continue;
            };
            if !matches!(
                (module.as_str(), name.as_str()),
                ("django.urls", "reverse" | "reverse_lazy") | ("django.shortcuts", "redirect")
            ) {
                continue;
            }
            if let Some(key) = argument(&call, 0, "viewname") {
                if key.syntax().text_range().contains(offset) {
                    let name = literal(key)?;
                    if registrations.iter().filter(|url| url.name == name).count() == 1 {
                        return Some(FrameworkTarget::Url { name });
                    }
                }
            }
        }
    }
    None
}
pub fn rename_target(
    db: &Analysis,
    target: &FrameworkTarget,
    new: &str,
    paths: &[PathBuf],
    overrides: &BTreeMap<PathBuf, String>,
) -> Result<Vec<(PathBuf, Vec<Edit>)>, String> {
    crate::rename::check_identifier(new)?;
    if new == target.name() {
        return Err("The declaration already has that name".into());
    }
    if let FrameworkTarget::Context {
        path,
        name,
        source,
        key,
    } = target
    {
        let (file, context, expression) = context_key(db, path, name, paths)
            .ok_or("The render context is ambiguous or changed")?;
        if file.path != *source
            || crate::rename_family::literal_name_range(&file, &expression, name) != Some(*key)
        {
            return Err("The render declaration changed; reopen Rename".into());
        }
        if context
            .items()
            .filter_map(|item| item.key())
            .any(|key| literal(key).as_deref() == Some(new))
        {
            return Err("That context key already exists".into());
        }
        let text = overrides
            .get(path)
            .cloned()
            .or_else(|| text(db, path))
            .ok_or("Template is unavailable")?;
        let document = template::parse(&text, Dialect::Django);
        return Ok(vec![
            (source.clone(), vec![(*key, new.to_owned())]),
            (
                path.clone(),
                root_uses(&document, name)
                    .into_iter()
                    .map(|range| (range, new.to_owned()))
                    .collect(),
            ),
        ]);
    }
    if let FrameworkTarget::Url { name } = target {
        return Ok(url_edits(db, &urls(db, paths), name, new, paths)?
            .into_iter()
            .collect());
    }
    let FrameworkTarget::Block { path, name } = target else {
        unreachable!()
    };
    let mut documents = BTreeMap::new();
    let mut bytes = 0;
    for candidate in template_paths(paths)
        .into_iter()
        .chain(std::iter::once(path.clone()))
    {
        if documents.contains_key(&candidate) {
            continue;
        }
        if documents.len() >= 512 {
            return Err("Choose fewer than 512 related templates".into());
        }
        let source = overrides
            .get(&candidate)
            .cloned()
            .or_else(|| text(db, &candidate))
            .ok_or("Cannot read a related template")?;
        bytes += source.len();
        if bytes > 8 * 1024 * 1024 {
            return Err("Related templates exceed the 8 MiB rename limit".into());
        }
        documents.insert(candidate, source);
    }
    let mut edges = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    for (candidate, source) in &documents {
        let document = template::parse(source, Dialect::Django);
        for region in &document.regions {
            if region.kind != Kind::Statement
                || !source[region.content.clone()]
                    .trim_start()
                    .starts_with("extends ")
            {
                continue;
            }
            let names = quoted_ranges(source, region.content.clone());
            if names.is_empty() && candidate == path {
                return Err("Dynamic template inheritance cannot identify the block family".into());
            }
            if let Some((_, name)) = names.first() {
                let parents = documents
                    .keys()
                    .filter(|path| path.ends_with(name))
                    .collect::<Vec<_>>();
                if parents.len() == 1 {
                    let parent = parents[0];
                    edges
                        .entry(candidate.clone())
                        .or_default()
                        .push(parent.clone());
                    edges
                        .entry(parent.clone())
                        .or_default()
                        .push(candidate.clone());
                } else if candidate == path {
                    return Err("The base template is ambiguous or unavailable".into());
                }
            }
        }
    }
    let mut family = BTreeSet::new();
    let mut pending = vec![path.clone()];
    while let Some(path) = pending.pop() {
        if family.insert(path.clone()) {
            pending.extend(edges.get(&path).into_iter().flatten().cloned());
        }
    }
    let mut result = Vec::new();
    for path in family {
        let source = &documents[&path];
        let document = template::parse(source, Dialect::Django);
        let mut edits = Vec::new();
        for region in &document.regions {
            if region.kind != Kind::Statement {
                continue;
            }
            let words = identifier_ranges(source, region.content.clone());
            if words
                .first()
                .is_some_and(|(_, word)| matches!(word.as_str(), "block" | "endblock"))
            {
                if let Some((range, current)) = words.get(1) {
                    if current == new {
                        return Err("The related template already declares that block name".into());
                    }
                    if current == name {
                        edits.push((rng(range.start, range.end), new.to_owned()));
                    }
                }
            }
        }
        if !edits.is_empty() {
            result.push((path, edits));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn setup(files: &[(&str, &str)]) -> (Analysis, Vec<PathBuf>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let paths = files
            .iter()
            .map(|(name, source)| {
                let path = PathBuf::from(format!("/proj/{name}"));
                db.set_file_text(&path, source);
                path
            })
            .collect();
        (db, paths)
    }
    fn changed(edits: &[(PathBuf, Vec<Edit>)], name: &str, source: &str) -> String {
        let edits = edits
            .iter()
            .find(|(path, _)| path.ends_with(name))
            .map(|(_, edits)| edits.as_slice())
            .unwrap_or_default();
        rustpy_semantic::edit_helpers::apply_edits(source, edits)
    }
    #[test]
    fn template_context_roots_respect_loop_with_and_attribute_scopes() {
        let source = "{{ user }} {{ item.user }} {% if user %}{{ user }}{% endif %}{% for user in users %}{{ user }}{% endfor %}{% with user=other %}{{ user }}{% endwith %}{{ user }}";
        let document = template::parse(source, Dialect::Django);
        let edits = root_uses(&document, "user")
            .into_iter()
            .map(|range| (range, "account".into()))
            .collect::<Vec<_>>();
        let result = rustpy_semantic::edit_helpers::apply_edits(source, &edits);
        assert!(
            result.starts_with("{{ account }} {{ item.user }} {% if account %}{{ account }}"),
            "{result}"
        );
        assert!(
            result.contains("{% for user in users %}{{ user }}{% endfor %}")
                && result.contains("{% with user=other %}{{ user }}{% endwith %}"),
            "{result}"
        );
        assert!(result.ends_with("{{ account }}"));
    }
    #[test]
    fn block_family_uses_actual_literal_extends_and_named_endblocks() {
        let base = "{% block content %}base{% endblock content %}";
        let child = "{% extends 'base.html' %}{% block content %}child{% endblock content %}";
        let unrelated = "{% block content %}other{% endblock content %}";
        let (db, paths) = setup(&[
            ("templates/base.html", base),
            ("templates/child.html", child),
            ("other.html", unrelated),
        ]);
        let target = target_at(
            &db,
            &paths[1],
            child,
            TextSize::new(child.find("content").unwrap() as u32),
            &paths,
        )
        .unwrap();
        let edits = rename_target(&db, &target, "body", &paths, &BTreeMap::new()).unwrap();
        assert_eq!(
            changed(&edits, "base.html", base),
            "{% block body %}base{% endblock body %}"
        );
        assert!(changed(&edits, "child.html", child).contains("{% endblock body %}"));
        assert_eq!(changed(&edits, "other.html", unrelated), unrelated);
        assert!(rename_target(&db, &target, "invalid-name", &paths, &BTreeMap::new()).is_err());
    }
    #[test]
    fn dynamic_or_ambiguous_template_parent_is_not_a_guessed_family() {
        let source = "{% extends selected_template %}{% block content %}x{% endblock %}";
        let (db, paths) = setup(&[("child.html", source)]);
        let target = FrameworkTarget::Block {
            path: paths[0].clone(),
            name: "content".into(),
        };
        assert!(
            rename_target(&db, &target, "body", &paths, &BTreeMap::new())
                .unwrap_err()
                .contains("Dynamic")
        );
    }
    #[test]
    fn canonical_url_alias_and_namespace_references_follow_registration_identity() {
        let source = "from django.urls import path as route, reverse as link\ndef profile(request): pass\napp_name = 'accounts'\nurlpatterns = [route('profile/', profile, name='profile')]\nurl = link('accounts:profile')\n";
        let template = "{% url 'accounts:profile' %} {% url 'unrelated:profile' %}";
        let (db, paths) = setup(&[("urls.py", source), ("templates/index.html", template)]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let symbol = usages::symbol_at(
            &db,
            &file,
            TextSize::new(source.find("profile(request)").unwrap() as u32),
        )
        .unwrap();
        let edits = related_edits(&db, &symbol, "account", &paths).unwrap();
        let result = changed(&edits, "urls.py", source);
        assert!(
            result.contains("name='account'") && result.contains("link('accounts:account')"),
            "{result}"
        );
        assert_eq!(
            changed(&edits, "index.html", template),
            "{% url 'accounts:account' %} {% url 'unrelated:profile' %}"
        );
        let selected = target_at(
            &db,
            &paths[1],
            template,
            TextSize::new(template.find("accounts:profile").unwrap() as u32),
            &paths,
        )
        .unwrap();
        assert_eq!(selected.name(), "profile");
    }
    #[test]
    fn source_render_context_changes_dictionary_key_and_unshadowed_root_only() {
        let source = "from django.shortcuts import render\nuser = 1\nresponse = render(request, 'index.html', {'user': user})\n";
        let template = "{{ user }} {{ thing.user }} {% for user in users %}{{ user }}{% endfor %}";
        let (db, paths) = setup(&[("views.py", source), ("templates/index.html", template)]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let symbol = usages::symbol_at(
            &db,
            &file,
            TextSize::new(source.find("user =").unwrap() as u32),
        )
        .unwrap();
        let edits = related_edits(&db, &symbol, "account", &paths).unwrap();
        assert!(changed(&edits, "views.py", source).contains("{'account': user}"));
        assert_eq!(
            changed(&edits, "index.html", template),
            "{{ account }} {{ thing.user }} {% for user in users %}{{ user }}{% endfor %}"
        );
        let ambiguous = "from django.shortcuts import render\nuser = 1\nrender(request, 'index.html', {'user': user})\nrender(request, 'index.html', {'user': other})\n";
        db.set_file_text(&paths[0], ambiguous);
        let file = db.file_for_path(&paths[0]).unwrap();
        let symbol = usages::symbol_at(
            &db,
            &file,
            TextSize::new(ambiguous.find("user =").unwrap() as u32),
        )
        .unwrap();
        assert!(related_edits(&db, &symbol, "account", &paths)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn template_root_rename_changes_context_declaration_without_renaming_value_symbol() {
        let source = "from django.shortcuts import render\naccount = 1\nrender(request, 'index.html', {'user': account})\n";
        let template = "{{ user }} {{ item.user }} {% if user %}yes{% endif %}";
        let (db, paths) = setup(&[("views.py", source), ("templates/index.html", template)]);
        let target = target_at(
            &db,
            &paths[1],
            template,
            TextSize::new(template.find("user").unwrap() as u32),
            &paths,
        )
        .unwrap();
        assert!(matches!(&target,FrameworkTarget::Context{name,..} if name=="user"));
        let edits = rename_target(&db, &target, "profile", &paths, &BTreeMap::new()).unwrap();
        assert_eq!(
            changed(&edits, "views.py", source),
            "from django.shortcuts import render\naccount = 1\nrender(request, 'index.html', {'profile': account})\n"
        );
        assert_eq!(
            changed(&edits, "index.html", template),
            "{{ profile }} {{ item.user }} {% if profile %}yes{% endif %}"
        );
    }
    #[test]
    fn fake_framework_calls_do_not_create_semantic_string_references() {
        let source =
            "def render(*args): pass\nuser = 1\nrender(request, 'index.html', {'user': user})\n";
        let (db, paths) = setup(&[("views.py", source), ("templates/index.html", "{{ user }}")]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let symbol = usages::symbol_at(
            &db,
            &file,
            TextSize::new(source.find("user =").unwrap() as u32),
        )
        .unwrap();
        assert!(related_edits(&db, &symbol, "account", &paths)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn context_root_identity_uses_utf8_byte_ranges_and_excludes_shadowed_members() {
        let source = "π {{ profile.name }} {{ item.profile.name }} {% for profile in people %}{{ profile.name }}{% endfor %}{{ profile.name }}";
        let document = template::parse(source, Dialect::Django);
        let roots = root_use_offsets(&document, "profile");
        let first = source.find("profile.name").unwrap();
        let last = source.rfind("profile.name").unwrap();
        assert_eq!(
            roots,
            BTreeSet::from([
                (first, first + "profile".len()),
                (last, last + "profile".len())
            ])
        );
        for (start, end) in roots {
            assert_eq!(&source[start..end], "profile");
        }
    }
}
