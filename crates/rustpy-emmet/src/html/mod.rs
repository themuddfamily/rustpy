//! HTML (markup) abbreviations: parse, unroll, resolve snippets, transform,
//! pretty-print.

mod format;
mod parse;
mod snippets;
mod tree;

use crate::expansion::{has_marker, Expansion};

/// Expands an HTML abbreviation such as `ul>li.item$*3`, `a[href=#]{Home}`,
/// `(header>nav)+main`, `table>.row*2>.cell*3`, `p>lorem10` or `!`.
///
/// Nested lines are indented with `indent` (e.g. `"\t"` or `"    "`). The
/// caret goes to the first empty attribute value, empty element or snippet
/// placeholder, or to the end when there is none. Returns `None` when `abbr`
/// is not a valid abbreviation (unbalanced brackets, stray whitespace,
/// operators without operands, tag names that are not names, more than
/// 10 000 generated elements, ...).
pub fn expand_html(abbr: &str, indent: &str) -> Option<Expansion> {
    expand_html_in(abbr, indent, None)
}

/// Like [`expand_html`], with `parent` naming the element the expansion will
/// be inserted into: it picks the implicit tag of top-level nameless
/// elements, so `.item` becomes `<li class="item"></li>` inside a `<ul>` and
/// `<span class="item"></span>` inside a `<p>`.
pub fn expand_html_in(abbr: &str, indent: &str, parent: Option<&str>) -> Option<Expansion> {
    expand_in(abbr, indent, parent, false)
}
pub(crate) fn expand_jsx_in(abbr: &str, indent: &str, parent: Option<&str>) -> Option<Expansion> {
    expand_in(abbr, indent, parent, true)
}
fn expand_in(abbr: &str, indent: &str, parent: Option<&str>, jsx: bool) -> Option<Expansion> {
    let abbr = abbr.trim();
    if abbr.is_empty() || has_marker(abbr) {
        return None;
    }
    let items = parse::parse(abbr).ok()?;
    let mut convert = tree::Convert::new(tree::MAX_NODES);
    let nodes = convert.items(&items)?;
    let mut budget = convert.budget;
    let mut nodes = tree::resolve_snippets(nodes, &mut Vec::new(), &mut budget)?;
    tree::Transform::new().run(&mut nodes, parent.filter(|p| !p.is_empty()))?;
    let rendered = if jsx {
        format::render_syntax(&nodes, indent, true)
    } else {
        format::render(&nodes, indent)
    };
    let expansion = Expansion::from_marked(&rendered);
    (!expansion.text.trim().is_empty()).then_some(expansion)
}

/// Surround raw selected text in every deepest non-void leaf. Selection is
/// never parsed as abbreviation syntax, preserving markup and Unicode bytes.
pub(crate) fn surround_html(
    abbr: &str,
    selection: &str,
    indent: &str,
    parent: Option<&str>,
    jsx: bool,
) -> Option<Expansion> {
    if selection.len() > 1024 * 1024 || has_marker(selection) {
        return None;
    }
    let items = parse::parse(abbr).ok()?;
    let mut convert = tree::Convert::new(tree::MAX_NODES);
    let nodes = convert.items(&items)?;
    let mut budget = convert.budget;
    let mut nodes = tree::resolve_snippets(nodes, &mut Vec::new(), &mut budget)?;
    tree::Transform::new().run(&mut nodes, parent)?;
    fn insert(nodes: &mut [tree::Node], selection: &str, bytes: &mut usize) -> Option<bool> {
        let mut inserted = false;
        for node in nodes {
            if !node.children.is_empty() {
                inserted |= insert(&mut node.children, selection, bytes)?;
            } else if node
                .name
                .as_deref()
                .is_some_and(|name| !format::is_void(name))
                && !node.self_closing
            {
                *bytes = bytes.checked_add(selection.len())?;
                if *bytes > 1024 * 1024 {
                    return None;
                }
                node.text
                    .get_or_insert_with(String::new)
                    .push_str(selection);
                inserted = true;
            }
        }
        Some(inserted)
    }
    if !insert(&mut nodes, selection, &mut 0)? {
        return None;
    }
    let result = Expansion::from_marked(&format::render_syntax(&nodes, indent, jsx));
    (result.text.len() <= 1024 * 1024).then_some(result)
}
