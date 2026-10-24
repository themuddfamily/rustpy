//! Pretty-printer for the output tree, following Emmet's HTML formatting:
//! block elements go on their own lines with children indented, inline
//! elements stay on the line unless three or more are adjacent or they sit
//! next to block elements, `<html>` children are not indented, an empty
//! `<body>` opens an indented line for the caret, and the children of a text
//! snippet with an edit point replace that edit point (`c>p` ->
//! `<!-- <p></p> -->`).

use std::borrow::Cow;

use super::tree::{Attr, Node};
use crate::expansion::{CARET, FIELD_CLOSE, FIELD_OPEN};

#[rustfmt::skip]
const INLINE: &[&str] = &[
    "a", "abbr", "acronym", "applet", "b", "basefont", "bdo", "big", "br", "button", "cite", "code", "del", "dfn",
    "em", "font", "i", "iframe", "img", "input", "ins", "kbd", "label", "map", "object", "q", "s", "samp", "select",
    "small", "span", "strike", "strong", "sub", "sup", "textarea", "tt", "u", "var",
];

#[rustfmt::skip]
const VOID: &[&str] = &[
    "area", "base", "basefont", "br", "col", "command", "embed", "frame", "hr", "img", "input", "isindex", "keygen",
    "link", "meta", "param", "source", "track", "wbr",
];

/// Written without a value when none is given (`<input disabled>`).
#[rustfmt::skip]
const BOOLEAN_ATTRS: &[&str] = &[
    "allowfullscreen", "async", "autofocus", "autoplay", "checked", "controls", "default", "defer", "disabled",
    "formnovalidate", "hidden", "inert", "ismap", "itemscope", "loop", "multiple", "muted", "nomodule",
    "novalidate", "open", "playsinline", "readonly", "required", "reversed", "selected", "typemustmatch",
];

/// Children are not indented inside these.
const FORMAT_SKIP: &[&str] = &["html"];
/// An empty element of this kind gets its caret on an indented line of its own.
const FORMAT_FORCE: &[&str] = &["body"];
/// This many adjacent inline elements are put on separate lines.
const INLINE_BREAK: usize = 3;

pub(crate) fn is_inline(name: &str) -> bool {
    INLINE.contains(&name.to_ascii_lowercase().as_str())
}

pub(crate) fn is_void(name: &str) -> bool {
    VOID.contains(&name.to_ascii_lowercase().as_str())
}

/// Renders `nodes` with `indent` as the indentation unit. The result still
/// contains field markers.
pub(crate) fn render(nodes: &[Node], indent: &str) -> String {
    render_syntax(nodes, indent, false)
}
pub(crate) fn render_syntax(nodes: &[Node], indent: &str, jsx: bool) -> String {
    let mut f = Formatter {
        out: String::new(),
        indent,
        jsx,
    };
    f.walk(nodes, None, 0, 0);
    f.out
}

fn is_text_node(n: &Node) -> bool {
    n.name.is_none()
}

/// Inline-level: an inline element, or a text-only node.
fn is_inline_node(n: &Node) -> bool {
    match &n.name {
        Some(name) => is_inline(name),
        None => n.text.is_some() && n.attrs.is_empty(),
    }
}

fn is_inline_element(n: &Node) -> bool {
    n.name.as_deref().is_some_and(is_inline)
}

/// Whether `node` (the `i`-th of `items`) starts on a new line.
fn should_format(node: &Node, i: usize, items: &[Node], parent: Option<&Node>) -> bool {
    if i == 0 && parent.is_none() {
        return false;
    }
    if parent.is_some_and(is_text_node) && items.len() == 1 {
        return false;
    }
    if is_text_node(node) {
        let text = node.text.as_deref().unwrap_or("");
        let adjacent_text =
            (i > 0 && is_text_node(&items[i - 1])) || items.get(i + 1).is_some_and(is_text_node);
        if adjacent_text
            || text.contains('\n')
            || (text.contains(FIELD_OPEN) && !node.children.is_empty())
        {
            return true;
        }
    }
    if !is_inline_node(node) {
        return true;
    }
    if i == 0 {
        if items.iter().any(|n| !is_inline_node(n)) {
            return true;
        }
    } else if !is_inline_node(&items[i - 1]) {
        return true;
    }
    // Capped: only "at least INLINE_BREAK in a row" matters, and long runs
    // (`span*5000`) would otherwise make this quadratic.
    let before = items[..i]
        .iter()
        .rev()
        .take_while(|n| is_inline_element(n))
        .take(INLINE_BREAK)
        .count();
    let after = items[i + 1..]
        .iter()
        .take_while(|n| is_inline_element(n))
        .take(INLINE_BREAK)
        .count();
    if 1 + before + after >= INLINE_BREAK {
        return true;
    }
    node.children
        .iter()
        .enumerate()
        .any(|(j, c)| should_format(c, j, &node.children, Some(node)))
}

/// `<p>...` style content: does the text open with a block-level tag?
fn starts_with_block_tag(text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('<') else {
        return false;
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    !name.is_empty() && name.starts_with(|c: char| c.is_ascii_alphabetic()) && !is_inline(&name)
}

/// Splits `text` around its first (outermost) edit point: the text before
/// the field and the text after its closing marker.
fn split_at_first_field(text: &str) -> Option<(&str, &str)> {
    let open = text.find(FIELD_OPEN)?;
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        if c == FIELD_OPEN {
            depth += 1;
        } else if c == FIELD_CLOSE {
            depth -= 1;
            if depth == 0 {
                return Some((&text[..open], &text[open + i + c.len_utf8()..]));
            }
        }
    }
    None
}

struct Formatter<'a> {
    jsx: bool,
    out: String,
    indent: &'a str,
}

impl Formatter<'_> {
    fn newline(&mut self, level: usize) {
        self.out.push('\n');
        for _ in 0..level {
            self.out.push_str(self.indent);
        }
    }

    /// Pushes text, indenting continuation lines to `level`.
    fn text(&mut self, text: &str, level: usize) {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.newline(level);
            }
            self.out.push_str(line);
        }
    }

    /// `nodes` sit at `level`; their parent (if any) was printed at `parent_level`.
    fn walk(&mut self, nodes: &[Node], parent: Option<&Node>, level: usize, parent_level: usize) {
        for (i, node) in nodes.iter().enumerate() {
            let format = should_format(node, i, nodes, parent);
            if format {
                self.newline(level);
            }
            self.node(node, level);
            if format && i + 1 == nodes.len() && parent.is_some() {
                // The parent's closing tag goes on its own line.
                self.newline(parent_level);
            }
        }
    }

    fn node(&mut self, node: &Node, level: usize) {
        let Some(name) = &node.name else {
            self.text_node(node, level);
            return;
        };
        self.out.push('<');
        self.out.push_str(name);
        for attr in &node.attrs {
            self.attr(attr);
        }
        if self.jsx
            && (node.self_closing || is_void(name))
            && node.children.is_empty()
            && node.text.is_none()
        {
            self.out.push_str(" />");
        } else {
            self.out.push('>');
        }
        if (node.self_closing || is_void(name)) && node.children.is_empty() && node.text.is_none() {
            return;
        }
        if let Some(text) = &node.text {
            let inner = text.contains('\n') || starts_with_block_tag(text);
            if inner {
                self.newline(level + 1);
            }
            self.text(text, if inner { level + 1 } else { level });
            if inner {
                self.newline(level);
            }
        }
        let lower = name.to_ascii_lowercase();
        let child_level = if FORMAT_SKIP.contains(&lower.as_str()) {
            level
        } else {
            level + 1
        };
        self.walk(&node.children, Some(node), child_level, level);
        if node.text.is_none() && node.children.is_empty() {
            let inner = FORMAT_FORCE.contains(&lower.as_str());
            if inner {
                self.newline(level + 1);
            }
            self.out.push_str(CARET);
            if inner {
                self.newline(level);
            }
        }
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
    }

    /// A text-only node (text block, comment snippet, lorem). Its children
    /// go where its first edit point is, as in Emmet (`c>p` puts the
    /// paragraph inside the comment), or after the text when it has none.
    fn text_node(&mut self, node: &Node, level: usize) {
        let text = node.text.as_deref().unwrap_or("");
        let Some((before, after)) =
            split_at_first_field(text).filter(|_| !node.children.is_empty())
        else {
            self.text(text, level);
            self.walk(&node.children, Some(node), level, level);
            return;
        };
        self.text(before, level);
        let mark = self.out.len();
        self.walk(&node.children, Some(node), level, level);
        let mut after = after;
        if self.out[mark..].contains('\n') {
            // The children went onto lines of their own: drop the spaces that
            // padded the field (`<!-- ` / ` -->`) at the line breaks.
            if self.out[mark..].starts_with('\n') {
                let trailing = before.len() - before.trim_end_matches([' ', '\t']).len();
                self.out.replace_range(mark - trailing..mark, "");
            }
            after = after.trim_start_matches([' ', '\t']);
        }
        self.text(after, level);
    }

    fn attr(&mut self, attr: &Attr) {
        self.out.push(' ');
        self.out.push_str(if self.jsx {
            match attr.name.as_str() {
                "class" => "className",
                "for" => "htmlFor",
                "tabindex" => "tabIndex",
                "readonly" => "readOnly",
                "autofocus" => "autoFocus",
                name => name,
            }
        } else {
            &attr.name
        });
        match attr.value.as_deref() {
            Some(v) if !v.is_empty() => {
                let (quote, value) = match (v.contains('"'), v.contains('\'')) {
                    (true, false) => ('\'', Cow::Borrowed(v)),
                    (true, true) => ('"', Cow::Owned(v.replace('"', "&quot;"))),
                    _ => ('"', Cow::Borrowed(v)),
                };
                self.out.push('=');
                self.out.push(quote);
                self.out.push_str(&value);
                self.out.push(quote);
            }
            _ if attr.boolean
                || BOOLEAN_ATTRS.contains(&attr.name.to_ascii_lowercase().as_str()) => {}
            _ => {
                self.out.push_str("=\"");
                self.out.push_str(CARET);
                self.out.push('"');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_void_tables() {
        assert!(is_inline("SPAN"));
        assert!(!is_inline("div"));
        assert!(is_void("br"));
        assert!(!is_void("p"));
        assert!(starts_with_block_tag("<div>x</div>"));
        assert!(!starts_with_block_tag("<b>x</b>"));
        assert!(!starts_with_block_tag("plain"));
    }

    #[test]
    fn first_field_split() {
        let text = format!("<!-- {} -->", crate::expansion::field("x"));
        assert_eq!(split_at_first_field(&text), Some(("<!-- ", " -->")));
        let nested = format!("a{FIELD_OPEN}b{CARET}c{FIELD_CLOSE}d{CARET}");
        assert_eq!(
            split_at_first_field(&nested),
            Some(("a", format!("d{CARET}").as_str()))
        );
        assert_eq!(split_at_first_field("plain"), None);
    }
}
