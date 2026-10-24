//! From parsed abbreviation to output tree: repeats are unrolled and `$`
//! numbering evaluated (`convert`), snippets expanded (`resolve_snippets`),
//! then implicit tag names, attribute merging, `lorem` text and the
//! `label>input` cleanup are applied (`transform`).

use super::format::is_inline;
use super::parse::{self, Item, Piece, Pieces};
use super::snippets;
use crate::expansion::{has_marker, is_blank_value, FIELD_CLOSE, FIELD_OPEN};
use crate::lorem;

/// Most nodes a single expansion may produce (guards `div*9999>p*9999`).
pub(crate) const MAX_NODES: usize = 10_000;

#[derive(Clone, Debug, Default)]
pub(crate) struct Node {
    /// `None` for a text-only node.
    pub name: Option<String>,
    pub attrs: Vec<Attr>,
    /// Element content (or the text of a text-only node); may contain field markers.
    pub text: Option<String>,
    pub children: Vec<Node>,
    pub self_closing: bool,
    /// Set when the node itself carried `*N`: `(index, count)` of this copy.
    pub repeat: Option<(usize, usize)>,
    /// Index of this copy within the innermost enclosing repeat, if any.
    pub rep_index: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attr {
    /// Empty for a value-only attribute (`["value"]`) until it is merged away.
    pub name: String,
    pub value: Option<String>,
    pub boolean: bool,
}

/// Unrolls repeats and evaluates numbering for a parsed abbreviation.
pub(crate) struct Convert {
    repeaters: Vec<(usize, usize)>,
    pub(crate) budget: usize,
}

impl Convert {
    pub(crate) fn new(budget: usize) -> Convert {
        Convert {
            repeaters: Vec::new(),
            budget,
        }
    }

    pub(crate) fn items(&mut self, items: &[Item]) -> Option<Vec<Node>> {
        let mut out = Vec::new();
        for item in items {
            out.extend(self.item(item)?);
        }
        Some(out)
    }

    fn item(&mut self, item: &Item) -> Option<Vec<Node>> {
        let repeat = match item {
            Item::Element(e) => e.repeat,
            Item::Group { repeat, .. } => *repeat,
        };
        let Some(parse::Repeat { count }) = repeat else {
            return self.once(item, None);
        };
        let mut out = Vec::new();
        for index in 0..count {
            self.repeaters.push((index, count));
            let nodes = self.once(item, Some((index, count)));
            self.repeaters.pop();
            out.extend(nodes?);
        }
        Some(out)
    }

    fn once(&mut self, item: &Item, own_repeat: Option<(usize, usize)>) -> Option<Vec<Node>> {
        let e = match item {
            Item::Group { items, .. } => return self.items(items),
            Item::Element(e) => e,
        };
        self.budget = self.budget.checked_sub(1)?;
        let name = match &e.name {
            Some(p) => Some(render(p, &self.repeaters, false)?),
            None => None,
        };
        let text = match &e.text {
            Some(p) => Some(render(p, &self.repeaters, true)?).filter(|t| !t.is_empty()),
            None => None,
        };
        let mut attrs = Vec::with_capacity(e.attrs.len());
        for a in &e.attrs {
            let value = match &a.value {
                Some(v) => Some(render(v, &self.repeaters, true)?),
                None => None,
            };
            attrs.push(Attr {
                name: render(&a.name, &self.repeaters, false)?,
                value,
                boolean: a.boolean,
            });
        }
        let rep_index = self.repeaters.last().map(|r| r.0);
        let children = self.items(&e.children)?;
        let mut node = Node {
            name,
            attrs,
            text,
            children: Vec::new(),
            self_closing: e.self_close,
            repeat: own_repeat,
            rep_index,
        };
        // A plain text node can't contain anything: its children follow it.
        if node.name.is_none()
            && node.attrs.is_empty()
            && node.text.as_deref().is_some_and(|t| !has_marker(t))
        {
            let mut out = vec![node];
            out.extend(children);
            return Some(out);
        }
        node.children = children;
        Some(vec![node])
    }
}

/// Renders pieces against the active repeaters (innermost last).
fn render(pieces: &Pieces, repeaters: &[(usize, usize)], allow_fields: bool) -> Option<String> {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(t) => out.push_str(t),
            Piece::Number {
                size,
                reverse,
                base,
                parent,
            } => {
                let (index, count) = repeaters
                    .len()
                    .checked_sub(1 + parent)
                    .map_or((0, 1), |i| repeaters[i]);
                let n = if *reverse {
                    base.saturating_add(count - index - 1)
                } else {
                    base.saturating_add(index)
                };
                out.push_str(&format!("{n:0size$}", size = *size));
            }
            Piece::Field(placeholder) => {
                if !allow_fields {
                    return None;
                }
                out.push(FIELD_OPEN);
                out.push_str(placeholder);
                out.push(FIELD_CLOSE);
            }
        }
    }
    Some(out)
}

/// Replaces nodes named after snippets with the snippet's (recursively
/// resolved) nodes: the node's attributes are appended to the snippet's top
/// elements, its text/self-closing flag/repeat carried over, and its children
/// moved into the snippet's deepest last node. Text-only top nodes (the
/// doctype of `!`) keep their text unless the snippet is nothing but text
/// (`c{note}` fills the comment's edit point).
pub(crate) fn resolve_snippets(
    nodes: Vec<Node>,
    stack: &mut Vec<&'static str>,
    budget: &mut usize,
) -> Option<Vec<Node>> {
    let mut out = Vec::with_capacity(nodes.len());
    for mut node in nodes {
        let children = std::mem::take(&mut node.children);
        let snippet = node
            .name
            .as_deref()
            .and_then(snippets::lookup)
            .filter(|s| !stack.contains(s));
        let Some(snippet) = snippet else {
            node.children = resolve_snippets(children, stack, budget)?;
            out.push(node);
            continue;
        };
        let items = parse::parse(snippet).ok()?;
        // The snippet's nodes replace this one, which was already counted.
        let mut convert = Convert::new(*budget + 1);
        let parsed = convert.items(&items)?;
        *budget = convert.budget;
        stack.push(snippet);
        let resolved = resolve_snippets(parsed, stack, budget);
        stack.pop();
        let mut resolved = resolved?;
        let has_element = resolved.iter().any(|n| n.name.is_some());
        for top in &mut resolved {
            if has_element && top.name.is_none() {
                continue;
            }
            top.attrs.extend(node.attrs.iter().cloned());
            top.self_closing |= node.self_closing;
            if let Some(text) = &node.text {
                if top.name.is_none() && top.text.as_deref().is_some_and(has_marker) {
                    top.children.insert(
                        0,
                        Node {
                            text: Some(text.clone()),
                            ..Node::default()
                        },
                    );
                } else {
                    top.text = Some(text.clone());
                }
            }
            if node.repeat.is_some() {
                top.repeat = node.repeat;
            }
            if node.rep_index.is_some() {
                top.rep_index = node.rep_index;
            }
        }
        let children = resolve_snippets(children, stack, budget)?;
        match deepest_last(&mut resolved) {
            Some(deepest) => deepest.children.extend(children),
            None => resolved.extend(children),
        }
        out.extend(resolved);
    }
    Some(out)
}

fn deepest_last(nodes: &mut [Node]) -> Option<&mut Node> {
    let mut node = nodes.last_mut()?;
    while !node.children.is_empty() {
        node = node.children.last_mut().expect("checked non-empty");
    }
    Some(node)
}

/// Elements whose implicit child tag is not `div`/`span`.
fn implicit_child(parent: &str) -> Option<&'static str> {
    Some(match parent {
        "p" => "span",
        "ul" | "ol" | "menu" => "li",
        "table" | "tbody" | "thead" | "tfoot" => "tr",
        "tr" => "td",
        "select" | "optgroup" | "datalist" => "option",
        "colgroup" => "col",
        "audio" | "video" => "source",
        "object" => "param",
        "map" => "area",
        _ => return None,
    })
}

/// The tag name for a nameless element (`.item`, `#id`, `[attr]`) inside
/// `parent`: `li` in lists, `tr`/`td` in tables, `option` in selects, `span`
/// inside inline elements, `div` otherwise.
pub(crate) fn implicit_tag(parent: Option<&str>) -> &'static str {
    let parent = parent.map(str::to_ascii_lowercase).unwrap_or_default();
    implicit_child(&parent).unwrap_or(if is_inline(&parent) { "span" } else { "div" })
}

/// Most lorem words one expansion may generate (guards `p*5000>lorem5000`).
const MAX_TOTAL_WORDS: usize = 50_000;

/// State for the transform pass.
pub(crate) struct Transform {
    rng: lorem::Rng,
    words_left: usize,
}

impl Transform {
    pub(crate) fn new() -> Transform {
        Transform {
            rng: lorem::Rng::new(0x5EED),
            words_left: MAX_TOTAL_WORDS,
        }
    }

    /// Applies lorem generation, implicit tags, attribute merging and the
    /// label cleanup top-down. Returns `None` when a tag or attribute name is
    /// not valid markup, or too much lorem text is requested.
    pub(crate) fn run(&mut self, nodes: &mut [Node], parent: Option<&str>) -> Option<()> {
        for node in nodes.iter_mut() {
            if let Some((min, max)) = node.name.as_deref().and_then(lorem::parse_name) {
                let words = self.rng.range(min, max);
                self.words_left = self.words_left.checked_sub(words)?;
                let common = node.rep_index.unwrap_or(0) == 0;
                node.text = Some(lorem::paragraph(words, common, &mut self.rng));
                node.name = None;
                // `ul>lorem*3` or `lorem.intro`: text wrapped in an implicit element.
                if !node.attrs.is_empty() || (node.repeat.is_some() && parent.is_some()) {
                    node.name = Some(implicit_tag(parent).to_string());
                }
            }
            if node.name.is_none() && !node.attrs.is_empty() {
                node.name = Some(implicit_tag(parent).to_string());
            }
            if let Some(name) = &node.name {
                if !valid_tag(name) {
                    return None;
                }
            }
            merge_attrs(&mut node.attrs);
            if node.attrs.iter().any(|a| !valid_attr(&a.name)) {
                return None;
            }
            if node.name.as_deref() == Some("label") {
                strip_label_for(node);
            }
            let name = node.name.clone();
            self.run(&mut node.children, name.as_deref().or(parent))?;
        }
        Some(())
    }
}

fn valid_tag(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

fn valid_attr(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '<' | '>' | '/' | '=' | FIELD_OPEN | FIELD_CLOSE
                )
        })
}

fn is_empty_attr(a: &Attr) -> bool {
    a.value.as_deref().is_none_or(is_blank_value)
}

/// De-duplicates attributes (the later value wins, the first position is
/// kept), joins repeated `class` values, and moves value-only attributes
/// (`a["url"]`) into the first attribute that has no value yet.
fn merge_attrs(attrs: &mut Vec<Attr>) {
    let mut merged: Vec<Attr> = Vec::with_capacity(attrs.len());
    let mut implied: Vec<String> = Vec::new();
    for attr in attrs.drain(..) {
        if attr.name.is_empty() {
            implied.extend(attr.value);
            continue;
        }
        match merged.iter_mut().find(|m| m.name == attr.name) {
            Some(prev) if attr.name == "class" => {
                prev.value = match (prev.value.take(), attr.value) {
                    (Some(a), Some(b)) if !is_blank_value(&a) && !is_blank_value(&b) => {
                        Some(format!("{a} {b}"))
                    }
                    (Some(a), b) if b.as_deref().is_none_or(is_blank_value) => Some(a),
                    (_, b) => b,
                };
            }
            Some(prev) => {
                prev.value = attr.value;
                prev.boolean |= attr.boolean;
            }
            None => merged.push(attr),
        }
    }
    for value in implied {
        if let Some(slot) = merged.iter_mut().find(|a| !a.boolean && is_empty_attr(a)) {
            slot.value = Some(value);
        }
    }
    *attrs = merged;
}

/// A `label` wrapping its control needs no (empty) `for`, nor the control an
/// (empty) `id`.
fn strip_label_for(label: &mut Node) {
    fn find_control(nodes: &mut [Node]) -> Option<&mut Node> {
        for n in nodes {
            if matches!(n.name.as_deref(), Some("input" | "textarea" | "select")) {
                return Some(n);
            }
            if let Some(found) = find_control(&mut n.children) {
                return Some(found);
            }
        }
        None
    }
    if let Some(control) = find_control(&mut label.children) {
        control
            .attrs
            .retain(|a| !(a.name == "id" && is_empty_attr(a)));
        label
            .attrs
            .retain(|a| !(a.name == "for" && is_empty_attr(a)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(abbr: &str) -> Vec<Node> {
        let items = parse::parse(abbr).unwrap();
        let mut budget = MAX_NODES;
        let mut convert = Convert::new(budget);
        let nodes = convert.items(&items).unwrap();
        budget = convert.budget;
        let mut nodes = resolve_snippets(nodes, &mut Vec::new(), &mut budget).unwrap();
        Transform::new().run(&mut nodes, None).unwrap();
        nodes
    }

    fn attr<'a>(n: &'a Node, name: &str) -> Option<&'a str> {
        n.attrs
            .iter()
            .find(|a| a.name == name)
            .and_then(|a| a.value.as_deref())
    }

    #[test]
    fn numbering() {
        let nodes = build("li.i$*3");
        let classes: Vec<_> = nodes.iter().map(|n| attr(n, "class").unwrap()).collect();
        assert_eq!(classes, ["i1", "i2", "i3"]);
        let nodes = build("li.i$$@-5*2");
        let classes: Vec<_> = nodes.iter().map(|n| attr(n, "class").unwrap()).collect();
        assert_eq!(classes, ["i06", "i05"]);
        // `$@^` reads the parent repeater.
        let nodes = build("ul*2>li.x$@^_$*2");
        assert_eq!(attr(&nodes[1].children[0], "class"), Some("x2_1"));
        assert_eq!(attr(&nodes[1].children[1], "class"), Some("x2_2"));
        // Outside any repeat a counter is its base.
        assert_eq!(attr(&build("p.c$@4")[0], "class"), Some("c4"));
    }

    #[test]
    fn snippet_chain_merges_attributes() {
        let nodes = build("input:text#q");
        let n = &nodes[0];
        assert_eq!(n.name.as_deref(), Some("input"));
        let names: Vec<_> = n.attrs.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["type", "name", "id"]);
        assert_eq!(attr(n, "type"), Some("text"));
        assert_eq!(attr(n, "id"), Some("q"));
        assert!(n.self_closing);
    }

    #[test]
    fn implicit_tags() {
        let tags = |abbr: &str| -> String {
            fn walk(n: &[Node], out: &mut Vec<String>) {
                for x in n {
                    out.push(x.name.clone().unwrap_or_default());
                    walk(&x.children, out);
                }
            }
            let mut out = Vec::new();
            walk(&build(abbr), &mut out);
            out.join(" ")
        };
        assert_eq!(tags(".a>.b"), "div div");
        assert_eq!(tags("ul>.a"), "ul li");
        assert_eq!(tags("table>.r>.c"), "table tr td");
        assert_eq!(tags("select>.o"), "select option");
        assert_eq!(tags("em>.x"), "em span");
        assert_eq!(tags("p>.x"), "p span");
    }

    #[test]
    fn rejects_bad_names() {
        let items = parse::parse("9lives").unwrap();
        let mut nodes = Convert::new(MAX_NODES).items(&items).unwrap();
        assert!(Transform::new().run(&mut nodes, None).is_none());
    }

    #[test]
    fn budget_limits_output() {
        let items = parse::parse("div*200>p*200").unwrap();
        assert!(Convert::new(MAX_NODES).items(&items).is_none());
        let items = parse::parse("p*5000>lorem5000").unwrap();
        let mut nodes = Convert::new(MAX_NODES).items(&items).unwrap();
        assert!(Transform::new().run(&mut nodes, None).is_none());
    }
}
