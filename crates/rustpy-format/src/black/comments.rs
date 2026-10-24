//! Port of the comment handling of `black/comments.py` (`# fmt:` directives
//! are handled by the caller, which keeps such files on the legacy path).

use super::nodes::is_type_comment_string;
use super::strings::{py_lstrip, py_rstrip};
use super::tree::*;

pub struct ProtoComment {
    pub ty: u16,
    pub value: String,
    pub newlines: usize,
    /// Bytes of the prefix consumed up to and including this comment's line.
    pub consumed: usize,
    pub form_feed: bool,
    /// Whitespace before the comment on its line.
    pub leading_whitespace: String,
}

pub fn make_simple_prefix(nl_count: usize, form_feed: bool) -> String {
    if form_feed {
        let mut s = "\n".repeat(nl_count.saturating_sub(1));
        s.push('\x0c');
        s.push('\n');
        return s;
    }
    "\n".repeat(nl_count)
}

pub fn list_comments(prefix: &str, is_endmarker: bool) -> Vec<ProtoComment> {
    let mut result = Vec::new();
    if prefix.is_empty() || !prefix.contains('#') {
        return result;
    }
    let mut consumed = 0usize;
    let mut nlines = 0usize;
    let mut ignored_lines = 0usize;
    let mut form_feed = false;
    for (index, full_line) in prefix.split('\n').enumerate() {
        consumed += full_line.len() + 1;
        let line = py_lstrip(full_line);
        if line.is_empty() {
            nlines += 1;
            if full_line.contains('\x0c') {
                form_feed = true;
            }
        }
        if !line.starts_with('#') {
            if line.ends_with('\\') {
                ignored_lines += 1;
            }
            continue;
        }
        let ty = if index == ignored_lines && !is_endmarker {
            COMMENT
        } else {
            STANDALONE_COMMENT
        };
        result.push(ProtoComment {
            ty,
            value: make_comment(line),
            newlines: nlines,
            consumed,
            form_feed,
            leading_whitespace: full_line[..full_line.len() - line.len()].to_string(),
        });
        form_feed = false;
        nlines = 0;
    }
    result
}

const COMMENT_EXCEPTIONS: &str = " !:#'";

pub fn make_comment(content: &str) -> String {
    let content = py_rstrip(content);
    if content.is_empty() {
        return "#".into();
    }
    if content.starts_with('#') && contains_fmt_directive(content) {
        return content.to_string();
    }
    let mut content: String = content.strip_prefix('#').unwrap_or(content).to_string();
    const NBSP: char = '\u{a0}';
    if content.starts_with(NBSP) && !is_type_comment_string(&format!("# {}", py_lstrip(&content))) {
        content = format!(" {}", &content[NBSP.len_utf8()..]);
    }
    if !content.is_empty()
        && !content.contains(NBSP)
        && is_type_comment_string(&format!("#{content}"))
    {
        if let Some((type_part, value_part)) = content.split_once(':') {
            content = format!("{}: {}", type_part.trim(), value_part.trim());
        }
    }
    if let Some(c) = content.chars().next() {
        if !COMMENT_EXCEPTIONS.contains(c) {
            content = format!(" {content}");
        }
    }
    format!("#{content}")
}

const ALL_DIRECTIVES: &[&str] = &[
    "# fmt: off",
    "# fmt:off",
    "# yapf: disable",
    "# fmt: skip",
    "# fmt:skip",
    "# fmt: on",
    "# fmt:on",
    "# yapf: enable",
];

/// Whether a comment line contains any `fmt:` / `yapf:` directive.
pub fn contains_fmt_directive(comment_line: &str) -> bool {
    contains_directive(comment_line, ALL_DIRECTIVES)
}

/// `contains_fmt_directive(comment_line, directives)`: alone, or within a
/// `# a # b` or `# a; b` list of comments.
pub fn contains_directive(comment_line: &str, directives: &[&str]) -> bool {
    if directives.contains(&comment_line) {
        return true;
    }
    for part in comment_line.split("# ").skip(1) {
        if directives.contains(&format!("# {}", part.trim()).as_str()) {
            return true;
        }
    }
    for part in comment_line
        .trim_matches(|c| c == '#' || c == ' ')
        .split(';')
    {
        if directives.contains(&format!("# {}", part.trim()).as_str()) {
            return true;
        }
    }
    false
}

/// `generate_comments(leaf)`: comment leaves from the prefix of `leaf`; the
/// prefix is then reduced to its newlines (`normalize_trailing_prefix`).
pub fn generate_comments(tree: &mut Tree, leaf: Id) -> Vec<Id> {
    let prefix = tree.n(leaf).prefix.clone();
    let comments = list_comments(&prefix, tree.ty(leaf) == ENDMARKER);
    let mut out = Vec::with_capacity(comments.len());
    let mut total_consumed = 0;
    for pc in comments {
        total_consumed = pc.consumed;
        let id = tree.leaf_with_prefix(
            pc.ty,
            pc.value,
            make_simple_prefix(pc.newlines, pc.form_feed),
        );
        out.push(id);
    }
    let remainder = if total_consumed < prefix.len() {
        &prefix[total_consumed..]
    } else {
        ""
    };
    let new_prefix = if !remainder.contains('\\') {
        let nl_count = remainder.matches('\n').count();
        let form_feed = remainder.contains('\x0c') && remainder.ends_with('\n');
        make_simple_prefix(nl_count, form_feed)
    } else {
        String::new()
    };
    tree.nm(leaf).prefix = new_prefix;
    out
}
