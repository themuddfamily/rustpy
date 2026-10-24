//! Port of Black's `# fmt: off` / `# fmt: on` / `# fmt: skip` handling
//! (`normalize_fmt_off` and its helpers in `black/comments.py`): the code a
//! directive switches off is replaced by a STANDALONE_COMMENT leaf holding
//! its source text verbatim, which the line generator then emits as is.

use super::comments::*;
use super::fxhash::HashSet;
use super::nodes::*;
use super::tree::*;

pub const FMT_OFF: &[&str] = &["# fmt: off", "# fmt:off", "# yapf: disable"];
pub const FMT_SKIP: &[&str] = &["# fmt: skip", "# fmt:skip"];
pub const FMT_ON: &[&str] = &["# fmt: on", "# fmt:on", "# yapf: enable"];

fn suffix(s: &str, from: usize) -> &str {
    if from >= s.len() {
        ""
    } else {
        &s[from..]
    }
}

fn prefix_upto(s: &str, to: usize) -> &str {
    &s[..to.min(s.len())]
}

/// Converts every `# fmt: off`/`on` region and `# fmt: skip` line.
pub fn normalize_fmt_off(tree: &mut Tree, root: Id) {
    let leaves = tree.leaves(root);
    let mut i = 0;
    // Each conversion consumes the directive it handled; the cap only guards
    // against looping forever on an unexpected tree shape.
    let mut budget = 4 * leaves.len() + 16;
    while i < leaves.len() && budget > 0 {
        i = convert_one_fmt_off_pair(tree, root, &leaves, i);
        budget -= 1;
    }
    preserve_directive_ownership(tree, root);
}

/// A logical expression can own comments on several physical lines. Black's
/// collapse/reparent operations may combine those comments even when a pragma
/// belongs to a particular physical expression. Preserve that statement using
/// the same verbatim CST leaf as fmt:skip, while formatting surrounding code.
fn preserve_directive_ownership(tree: &mut Tree, root: Id) {
    for statement in tree.pre_order(root) {
        if tree.ty(statement) != SIMPLE_STMT || !is_attached(tree, statement, root) {
            continue;
        }
        let comments = tree
            .leaves(statement)
            .into_iter()
            // The first token's prefix belongs before this statement. Its
            // first comment can look inline to list_comments (which has no
            // preceding-token context), but is actually standalone source.
            // Only later-token prefixes can own this statement's inline code.
            .skip(1)
            .flat_map(|leaf| list_comments(tree.prefix(leaf), tree.ty(leaf) == ENDMARKER))
            .filter(|comment| comment.ty == COMMENT)
            .collect::<Vec<_>>();
        if comments.len() < 2
            || !comments
                .iter()
                .any(|comment| physical_directive(&comment.value))
        {
            continue;
        }
        // Multiple type-ignore scopes keep the existing conservative safety
        // refusal; do not silently change how the semantic guard handles them.
        if comments
            .iter()
            .filter(|comment| comment.value.starts_with("# type: ignore"))
            .count()
            > 1
        {
            continue;
        }
        let prefix = tree.prefix(statement).to_string();
        tree.set_prefix(statement, "");
        let source = to_source(tree, statement);
        let preserved =
            tree.leaf_with_prefix(STANDALONE_COMMENT, source.trim_end_matches('\n'), prefix);
        tree.nm(preserved).fmt_first_leaf = tree.first_leaf(statement);
        tree.replace(statement, preserved);
    }
}

fn physical_directive(comment: &str) -> bool {
    if comment.starts_with("#!") {
        return true;
    }
    let lower = comment.to_ascii_lowercase();
    lower.contains("coding:")
        || lower.contains("coding=")
        || lower.split(['#', ';']).any(|part| {
            let body = part.trim_start();
            [
                "fmt:",
                "yapf:",
                "isort:",
                "type:",
                "noqa",
                "nosec",
                "noinspection",
                "pylint:",
                "pyright:",
                "mypy:",
                "ruff:",
            ]
            .iter()
            .any(|prefix| body.starts_with(prefix))
        })
}

fn is_attached(tree: &Tree, leaf: Id, root: Id) -> bool {
    let mut cur = Some(leaf);
    while let Some(c) = cur {
        if c == root {
            return true;
        }
        cur = tree.parent(c);
    }
    false
}

fn convert_one_fmt_off_pair(tree: &mut Tree, root: Id, leaves: &[Id], start: usize) -> usize {
    for (idx, &leaf) in leaves.iter().enumerate().skip(start) {
        if !is_attached(tree, leaf, root) || tree.ty(leaf) == STANDALONE_COMMENT {
            continue;
        }
        let mut previous_consumed = 0;
        let prefix = tree.n(leaf).prefix.clone();
        for comment in list_comments(&prefix, false) {
            let is_fmt_off = contains_directive(&comment.value, FMT_OFF);
            let is_fmt_skip = contains_directive(&comment.value, FMT_SKIP);
            let should_process =
                (is_fmt_off || is_fmt_skip) && !(is_fmt_off && is_closing(tree.ty(leaf)));
            if !should_process
                || !is_valid_standalone_fmt_comment(tree, &comment, leaf, is_fmt_off, is_fmt_skip)
            {
                previous_consumed = comment.consumed;
                continue;
            }
            let ignored = generate_ignored_nodes(tree, leaf, &comment);
            if ignored.is_empty() && is_fmt_off {
                if handle_comment_only_fmt_block(tree, leaf, previous_consumed) {
                    return idx;
                }
                continue;
            }
            if ignored.is_empty() {
                continue;
            }
            handle_regular_fmt_block(
                tree,
                &ignored,
                &comment,
                previous_consumed,
                is_fmt_skip,
                leaf,
            );
            return idx;
        }
    }
    leaves.len()
}

fn is_valid_standalone_fmt_comment(
    tree: &Tree,
    comment: &ProtoComment,
    leaf: Id,
    is_fmt_off: bool,
    is_fmt_skip: bool,
) -> bool {
    if comment.ty == STANDALONE_COMMENT {
        return true;
    }
    let Some(prev) = preceding_leaf(tree, Some(leaf)) else {
        return true;
    };
    let pt = tree.ty(prev);
    if is_fmt_off && !is_whitespace_token(pt) && pt != STANDALONE_COMMENT {
        return false;
    }
    if is_fmt_skip && is_whitespace_token(pt) {
        return false;
    }
    true
}

fn handle_comment_only_fmt_block(tree: &mut Tree, leaf: Id, previous_consumed: usize) -> bool {
    let original_prefix = tree.n(leaf).prefix.clone();
    let all = list_comments(&original_prefix, false);
    let mut off_idx = None;
    let mut on_idx = None;
    for (idx, c) in all.iter().enumerate() {
        if off_idx.is_none() && contains_directive(&c.value, FMT_OFF) {
            off_idx = Some(idx);
        }
        if let Some(o) = off_idx {
            if idx > o && contains_directive(&c.value, FMT_ON) {
                on_idx = Some(idx);
                break;
            }
        }
    }
    let (Some(off_idx), Some(on_idx)) = (off_idx, on_idx) else {
        return false;
    };
    let comment = &all[off_idx];
    let fmt_on = &all[on_idx];
    let between = &original_prefix
        [comment.consumed.min(original_prefix.len())..fmt_on.consumed.min(original_prefix.len())];
    let mut hidden = format!("{}\n{}", comment.value, between);
    if hidden.ends_with('\n') {
        hidden.pop();
    }
    let pre_consumed = if off_idx == 0 {
        previous_consumed
    } else {
        all[off_idx - 1].consumed
    };
    let mut preceding = prefix_upto(&original_prefix, pre_consumed).to_string();
    if off_idx > 0 {
        preceding = preceding.trim_start_matches(['\r', '\n']).to_string();
    }
    let mut standalone_prefix = preceding + &"\n".repeat(comment.newlines);
    let mut fmt_off_prefix = original_prefix
        .split(comment.value.as_str())
        .next()
        .unwrap_or("");
    if let Some(i) = fmt_off_prefix.rfind('\n') {
        fmt_off_prefix = &fmt_off_prefix[i + 1..];
    }
    standalone_prefix.push_str(fmt_off_prefix);
    tree.nm(leaf).prefix = suffix(&original_prefix, fmt_on.consumed).to_string();
    let Some(parent) = tree.parent(leaf) else {
        return true;
    };
    let index = tree.n(leaf).index as usize;
    let sc = tree.leaf_with_prefix(STANDALONE_COMMENT, hidden, standalone_prefix);
    tree.insert_child(parent, index, sc);
    true
}

fn is_fmt_on(tree: &Tree, container: Id) -> bool {
    let mut on = false;
    for c in list_comments(tree.prefix(container), false) {
        if contains_directive(&c.value, FMT_ON) {
            on = true;
        } else if contains_directive(&c.value, FMT_OFF) {
            on = false;
        }
    }
    on
}

fn children_contains_fmt_on(tree: &Tree, container: Id) -> bool {
    tree.children(container).iter().any(|c| {
        tree.first_leaf(*c)
            .map(|l| is_fmt_on(tree, l))
            .unwrap_or(false)
    })
}

/// `container_of`: the topmost ancestor whose first leaf is `leaf`.
fn container_of(tree: &Tree, leaf: Id) -> Id {
    let same_prefix = tree.prefix(leaf).to_string();
    let mut container = leaf;
    while let Some(parent) = tree.parent(container) {
        if tree.prefix(tree.child(parent, 0)) != same_prefix || tree.ty(parent) == FILE_INPUT {
            break;
        }
        if tree
            .prev_sibling(parent)
            .map(|s| is_bracket(tree.ty(s)))
            .unwrap_or(false)
        {
            break;
        }
        container = parent;
    }
    container
}

fn generate_ignored_nodes(tree: &mut Tree, leaf: Id, comment: &ProtoComment) -> Vec<Id> {
    if contains_directive(&comment.value, FMT_SKIP) {
        return generate_ignored_nodes_from_fmt_skip(tree, leaf, comment);
    }
    let mut out = Vec::new();
    let mut container = Some(container_of(tree, leaf));
    while let Some(c) = container {
        if tree.ty(c) == ENDMARKER || is_fmt_on(tree, c) {
            return out;
        }
        if children_contains_fmt_on(tree, c) {
            let children = tree.children(c).to_vec();
            for (index, &child) in children.iter().enumerate() {
                if tree.is_leaf(child) && is_fmt_on(tree, child) {
                    if is_closing(tree.ty(child)) {
                        // `# fmt: on` at a different bracket level: keep the bracket.
                        out.push(child);
                    }
                    return out;
                }
                if tree.ty(child) == INDENT
                    && index + 1 < children.len()
                    && children_contains_fmt_on(tree, children[index + 1])
                {
                    return out;
                }
                if children_contains_fmt_on(tree, child) {
                    return out;
                }
                out.push(child);
            }
            return out;
        }
        if tree.ty(c) == DEDENT && tree.next_sibling(c).is_none() {
            return out;
        }
        out.push(c);
        container = tree.next_sibling(c);
    }
    out
}

const COMPOUND_STATEMENTS: &[u16] = &[
    IF_STMT, WHILE_STMT, FOR_STMT, TRY_STMT, WITH_STMT, FUNCDEF, CLASSDEF, MATCH_STMT,
];

fn find_compound_statement_context(tree: &Tree, parent: Id) -> Option<Id> {
    if tree.ty(parent) != SIMPLE_STMT {
        return None;
    }
    let pp = tree.parent(parent)?;
    if tree.is_leaf(pp) {
        return None;
    }
    if tree.ty(pp) == SUITE {
        if let Some(ppp) = tree.parent(pp) {
            if !tree.is_leaf(ppp) && COMPOUND_STATEMENTS.contains(&tree.ty(ppp)) {
                return Some(pp);
            }
        }
    }
    if COMPOUND_STATEMENTS.contains(&tree.ty(pp)) {
        return Some(parent);
    }
    None
}

fn should_keep_compound_statement_inline(tree: &Tree, body: Id, simple_stmt_parent: Id) -> bool {
    let target = if tree.ty(body) == SUITE {
        let mut target = None;
        for &c in tree.children(body) {
            if tree.ty(c) == SIMPLE_STMT {
                if target.is_some() {
                    return false;
                }
                target = Some(c);
            }
        }
        match target {
            Some(t) if t == simple_stmt_parent => t,
            _ => return false,
        }
    } else {
        if body != simple_stmt_parent {
            return false;
        }
        body
    };
    tree.leaves(target).iter().any(|l| tree.ty(*l) == SEMI)
}

fn get_compound_statement_header(tree: &Tree, body: Id, simple_stmt_parent: Id) -> Vec<Id> {
    if !should_keep_compound_statement_inline(tree, body, simple_stmt_parent) {
        return Vec::new();
    }
    let Some(compound) = tree.parent(body) else {
        return Vec::new();
    };
    if !COMPOUND_STATEMENTS.contains(&tree.ty(compound)) {
        return Vec::new();
    }
    let mut header = Vec::new();
    if tree.parent_ty(compound) == Some(ASYNC_STMT) {
        if let Some(ps) = tree.prev_sibling(compound) {
            if tree.ty(ps) == ASYNC {
                header.push(ps);
            }
        }
    }
    for &child in tree.children(compound) {
        if child == body {
            break;
        }
        if tree.is_leaf(child) {
            if !matches!(tree.ty(child), NEWLINE | INDENT) {
                header.push(child);
            }
        } else {
            header.extend(tree.leaves(child));
        }
    }
    header
}

fn find_closest_previous_sibling(tree: &Tree, node: Id) -> Option<Id> {
    let mut cur = Some(node);
    while let Some(c) = cur {
        if let Some(p) = tree.prev_sibling(c) {
            return Some(p);
        }
        cur = tree.parent(c);
    }
    None
}

fn children_lead_ignored_nodes(tree: &Tree, node: Id, ignored: &[Id]) -> bool {
    let children = tree.children(node);
    !children.is_empty()
        && ignored.len() >= children.len()
        && ignored.iter().zip(children).all(|(a, b)| a == b)
}

fn generate_ignored_nodes_from_fmt_skip(
    tree: &mut Tree,
    leaf: Id,
    comment: &ProtoComment,
) -> Vec<Id> {
    let mut prev_sibling = tree.prev_sibling(leaf);
    let parent = tree.parent(leaf);
    let comments = list_comments(tree.prefix(leaf), false);
    if comments
        .first()
        .map(|c| c.value != comment.value)
        .unwrap_or(true)
    {
        return Vec::new();
    }
    if prev_sibling.is_none() {
        if let Some(p) = parent {
            prev_sibling = tree.prev_sibling(p);
        }
    }
    if prev_sibling.is_none() && comment.ty == COMMENT {
        prev_sibling = find_closest_previous_sibling(tree, leaf);
    }
    let mut ignored: Vec<Id> = Vec::new();
    if let (Some(p), true) = (parent, tree.ty(leaf) == NEWLINE) {
        if tree.ty(p) == SUITE {
            // `# fmt: skip` on the colon line of a compound statement header.
            tree.nm(leaf).prefix.clear();
            let mut ps = tree.prev_sibling(p);
            while let Some(s) = ps {
                if tree.ty(s) == SUITE {
                    break;
                }
                ignored.insert(0, s);
                ps = tree.prev_sibling(s);
            }
            if let Some(gp) = tree.parent(p) {
                if let Some(gps) = tree.prev_sibling(gp) {
                    if tree.ty(gps) == ASYNC {
                        ignored.insert(0, gps);
                    }
                }
            }
            return ignored;
        }
    }
    let Some(ps) = prev_sibling else {
        return Vec::new();
    };
    let mut current = ps;
    if tree.is_leaf(current)
        && is_opening(tree.ty(current))
        && tree.parent_ty(current) == Some(ATOM)
    {
        current = tree.parent(current).unwrap_or(current);
    }
    ignored.push(current);
    if tree.prev_sibling(current).is_none() {
        if let Some(p) = tree.parent(current) {
            current = p;
        }
    }
    let mut seen: HashSet<Id> = HashSet::default();
    seen.insert(current);
    while !tree.prefix(current).contains('\n') {
        let Some(ps) = tree.prev_sibling(current) else {
            break;
        };
        let next_node = tree.last_leaf(ps).unwrap_or(current);
        if seen.contains(&next_node) {
            break;
        }
        current = next_node;
        seen.insert(current);
        if tree.is_leaf(current) && tree.ty(current) == STANDALONE_COMMENT {
            break;
        }
        if is_closing(tree.ty(current)) && tree.parent_ty(current) == Some(ATOM) {
            current = tree.parent(current).unwrap_or(current);
        }
        if matches!(tree.ty(current), NEWLINE | INDENT) {
            if list_comments(tree.prefix(current), false).is_empty() {
                tree.set_prefix(current, "");
            }
            break;
        }
        if tree.ty(current) == DEDENT {
            break;
        }
        if let Some(p) = tree.parent(current) {
            if tree.ty(p) == ASEXPR_TEST && tree.parent_ty(p) == Some(WITH_STMT) {
                current = p;
            }
        }
        ignored.insert(0, current);
        if tree.prev_sibling(current).is_none() {
            if let Some(p) = tree.parent(current) {
                current = p;
                if children_lead_ignored_nodes(tree, current, &ignored) {
                    let n = tree.nchildren(current);
                    ignored.splice(0..n, [current]);
                    while tree.prev_sibling(current).is_none() {
                        let Some(pp) = tree.parent(current) else {
                            break;
                        };
                        if !children_lead_ignored_nodes(tree, pp, &ignored) {
                            break;
                        }
                        current = pp;
                        let n = tree.nchildren(current);
                        ignored.splice(0..n, [current]);
                    }
                }
            }
        }
    }
    if let Some(p) = parent {
        if !tree.is_leaf(p) {
            if let Some(body) = find_compound_statement_context(tree, p) {
                let header = get_compound_statement_header(tree, body, p);
                if !header.is_empty() {
                    let mut all = header;
                    all.extend(ignored);
                    ignored = all;
                }
            }
        }
    }
    let mut depth = 0i32;
    for &node in &ignored {
        for l in tree.leaves(node) {
            let t = tree.ty(l);
            if is_opening(t) {
                depth += 1;
            } else if is_closing(t) {
                depth -= 1;
            }
        }
    }
    if depth > 0 {
        // The skipped line opens a bracket: keep the whole statement.
        let mut statement = leaf;
        while let Some(p) = tree.parent(statement) {
            if matches!(tree.ty(p), FILE_INPUT | SUITE) {
                break;
            }
            statement = p;
        }
        ignored = vec![statement];
    }
    let leaf_is_ignored = ignored
        .iter()
        .any(|&ig| ig == leaf || (!tree.is_leaf(ig) && tree.leaves(ig).contains(&leaf)));
    if !leaf_is_ignored {
        let p = tree.n(leaf).prefix.clone();
        tree.nm(leaf).prefix = suffix(&p, comment.consumed).to_string();
    }
    ignored
}

fn remove_preceding_newline_for_comment(result: &str) -> String {
    let comment_line_start = result.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let Some(rel) = result[comment_line_start..].find('#') else {
        return result.to_string();
    };
    let comment_start = comment_line_start + rel;
    if let Some(nl) = result[..comment_start].rfind('\n') {
        if result[nl + 1..].trim_start().starts_with('#') {
            return format!("{}{}", &result[..nl], &result[nl + 1..]);
        }
    }
    result.to_string()
}

fn to_source(tree: &Tree, id: Id) -> String {
    let mut s = String::new();
    for l in tree.leaves(id) {
        let n = tree.n(l);
        s.push_str(&n.prefix);
        s.push_str(&n.value);
    }
    s
}

fn stringify_node(tree: &Tree, n: Id, next: Option<Id>) -> String {
    if tree.is_leaf(n) {
        if tree.ty(n) == STANDALONE_COMMENT {
            let node = tree.n(n);
            let mut result = format!("{}{}", node.prefix, node.value);
            if let Some(nx) = next {
                if tree.is_leaf(nx) && is_closing(tree.ty(nx)) && node.value.contains('\n') {
                    result = remove_preceding_newline_for_comment(&result);
                }
            }
            if !result.ends_with('\n') {
                result.push('\n');
            }
            return result;
        }
        return to_source(tree, n);
    }
    let children = tree.children(n);
    let mut out = String::new();
    for (i, &c) in children.iter().enumerate() {
        out.push_str(&stringify_node(tree, c, children.get(i + 1).copied()));
    }
    out
}

fn handle_regular_fmt_block(
    tree: &mut Tree,
    ignored: &[Id],
    comment: &ProtoComment,
    previous_consumed: usize,
    is_fmt_skip: bool,
    leaf: Id,
) {
    let first = ignored[0];
    let parent = tree.parent(first);
    let prefix = tree.prefix(first).to_string();
    let is_fmt_off = contains_directive(&comment.value, FMT_OFF);
    if is_fmt_off {
        tree.set_prefix(first, suffix(&prefix, comment.consumed));
    }
    let standalone_prefix = if is_fmt_skip {
        tree.set_prefix(first, "");
        prefix.clone()
    } else {
        format!(
            "{}{}",
            prefix_upto(&prefix, previous_consumed),
            "\n".repeat(comment.newlines)
        )
    };
    let mut hidden = String::new();
    for (i, &node) in ignored.iter().enumerate() {
        if tree.is_leaf(node) && tree.ty(node) == STANDALONE_COMMENT {
            let next = ignored.get(i + 1).copied();
            let mut s = to_source(tree, node);
            let needs_nl = match next {
                None => true,
                Some(nx) => {
                    (tree.is_leaf(nx) && tree.ty(nx) == STANDALONE_COMMENT)
                        || tree.prefix(nx).contains('\n')
                }
            };
            if !s.ends_with('\n') && needs_nl {
                s.push('\n');
            }
            hidden.push_str(&s);
        } else if !tree.is_leaf(node)
            && tree
                .leaves(node)
                .iter()
                .any(|l| tree.ty(*l) == STANDALONE_COMMENT)
        {
            hidden.push_str(&stringify_node(tree, node, None));
        } else {
            hidden.push_str(&to_source(tree, node));
        }
    }
    let leaf_is_ignored = ignored
        .iter()
        .any(|&ig| ig == leaf || (!tree.is_leaf(ig) && tree.leaves(ig).contains(&leaf)));
    if is_fmt_off {
        hidden = format!("{}\n{}", comment.value, hidden);
    }
    if is_fmt_skip && !leaf_is_ignored {
        hidden.push_str(&comment.leading_whitespace);
        hidden.push_str(&comment.value);
    }
    if hidden.ends_with('\n') {
        hidden.pop();
    }
    let mut first_idx = None;
    for &ig in ignored {
        let idx = tree.remove(ig);
        if first_idx.is_none() {
            first_idx = idx;
        }
    }
    let (Some(parent), Some(first_idx)) = (parent, first_idx) else {
        return;
    };
    let sc = tree.leaf_with_prefix(STANDALONE_COMMENT, hidden, standalone_prefix);
    tree.nm(sc).fmt_first_leaf = tree.first_leaf(first);
    tree.insert_child(parent, first_idx, sc);
}
